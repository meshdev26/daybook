#![allow(clippy::too_many_arguments)]
// Bot message formatting is intentional: plain-text labels (no emoji) keep
// messages stable across clients and accessible to screen readers. Emoji
// glyphs are avoided in bot output; status is conveyed via text.
use crate::{
    bots::{BotContext, BotManager},
    config::SharedConfig,
    crypto::CryptoPay,
    db::Db,
    dialogue::State,
    handlers::registry::CommandRegistry,
    roles, util,
};
use chrono::{Duration, Utc};
use std::sync::Arc;
use teloxide::{
    payloads::SendMessageSetters,
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId},
    utils::command::BotCommands,
};

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// Re-exports preserved after extraction (L11 refactor).
pub use crate::constants::MAX_PLAN_DAYS;
pub use crate::services::broadcast::filter_broadcast_users;

/// Active (non-revoked) MCP tokens a seller may hold at once (D6). The
/// per-seller cap is enforced HERE by counting `list_mcp_tokens` rows before
/// minting — the Db layer has no cap check, so this handler is the single
/// gate (coordinator decision; the spec copy quotes the same number).
const MCP_TOKEN_CAP: usize = 10;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
pub enum Command {
    /// Show help.
    #[command(aliases = ["h", "?"])]
    Help,
    /// Start / main menu.
    Start,
    /// Show configured target channel.
    Channel,
    /// List known channels.
    Channels,
    /// Set target channel: /setchannel @channel or id.
    SetChannel(String),
    /// Post text to channel: /post <text>.
    Post(String),
    /// Schedule a post: /schedule <@channel> <datetime> | <text>.
    Schedule(String),
    /// Post a photo (send photo next). /postphoto [caption].
    PostPhoto(String),
    /// Edit a posted message: /edit <msg_id> | <new text>.
    Edit(String),
    /// Delete a posted message: /delete [@channel] <msg_id>.
    Delete(String),
    /// Pin a message: /pin [@channel] <msg_id>.
    Pin(String),
    /// Unpin all messages in channel: /unpin [@channel].
    Unpin(String),
    /// Unpin specific message: /unpinmsg [@channel] <msg_id>.
    UnpinMsg(String),
    /// Create an invite link: /invite [@channel].
    Invite(String),
    /// Set channel description: /setdesc <text>.
    SetDesc(String),
    /// Set channel title: /settitle <text>.
    SetTitle(String),
    /// Channel info: /info [@channel].
    Info(String),
    /// Crypto Pay commands: /crypto <subcommand> ...
    Crypto(String),
    /// Inline menu.
    Menu,
    /// Bot self-setup: configure commands/profile. /setupbot
    SetupBot,
    /// Easy seller setup: /setup @channel
    Setup(String),
    /// Become a seller.
    BecomeSeller,
    /// List subscription plans. /plans
    Plans,
    /// Add plan: /planadd name | price | asset | days
    PlanAdd(String),
    /// Remove plan. /plandel name
    PlanDel(String),
    /// Add product: /productadd name | price | asset | kind | delivery | [stock]
    ProductAdd(String),
    /// List own products. /products
    Products,
    /// Deactivate a product. /productdel name
    ProductDel(String),
    /// List orders (sellers: own; admins: all). /orders
    Orders,
    /// Update an order: /orderset <order_id> <status> [tracking]
    OrderSet(String),
    /// Public shop listing. /shop
    Shop,
    /// Buy a plan or product: /buy name
    Buy(String),
    /// Show shopping cart. /cart
    Cart,
    /// Add to cart: /cartadd <name> [qty]
    CartAdd(String),
    /// Remove from cart: /cartremove <name>
    CartRemove(String),
    /// Clear cart. /cartclear
    CartClear,
    /// Checkout cart. /checkout
    Checkout,
    /// Claim payment: /paid invoice_id
    Paid(String),
    /// Redeem activation code: /redeem CODE
    Redeem(String),
    /// Generate codes: /gencodes plan | count | [days]
    GenCodes(String),
    /// List activation codes. /codes
    Codes,
    /// Grant access: /grant user_id | days
    Grant(String),
    /// Revoke access: /revoke user_id
    Revoke(String),
    /// List subscriptions. /subs
    Subs,
    /// Add service: /serviceadd name | price | asset | duration_minutes [| channel]
    ServiceAdd(String),
    /// List services. /services
    Services,
    /// Add slot: /slotadd service | datetime [| capacity]
    SlotAdd(String),
    /// Book a service: /book service_name
    Book(String),
    /// List bookings. /bookings
    Bookings,
    /// Your bookings: /mybookings
    MyBookings,
    /// Cancel your booking: /cancelbooking <booking_id>
    CancelBooking(String),
    /// Confirm a booking (seller): /bookingconfirm <booking_id>
    BookingConfirm(String),
    /// Decline a booking (seller): /bookingdecline <booking_id>
    BookingDecline(String),
    /// Run expiry cleanup. /expirecheck
    ExpireCheck,
    /// Broadcast to all users: /broadcast <text>.
    Broadcast(String),
    /// AI generate: /ai <template> | var=val | ...
    Ai(String),
    /// AI-assisted post: /aipost [@channel] <template> [| key=val ...].
    AiPost(String),
    /// Set web dashboard password: /setwebpassword <new>.
    SetWebPassword(String),
    /// Manage MCP API tokens: /mcptoken create <name> | list | revoke <id>.
    McpToken(String),
    /// Show web dashboard login info: /dashboard.
    Dashboard,
    /// Add platform admin (owner only): /addadmin <user_id> [username].
    AddAdmin(String),
    /// Remove platform admin (owner only): /removeadmin <user_id>.
    RemoveAdmin(String),
    /// List platform admins (owner only): /admins.
    Admins,
    /// List web accounts (owner only): /webusers.
    WebUsers,
    /// List sellers (admin/owner): /sellers.
    Sellers,
    /// Export buyer data: /exportdata <user_id> [seller_id].
    ExportData(String),
    /// Delete buyer data: /deletedata <user_id> [confirm] [seller_id].
    DeleteData(String),
    /// Approve a pending refund (admin/owner): /refundapprove [refund_id].
    RefundApprove(String),
    /// Approve a pending payout (admin/owner): /payoutapprove [payout_id].
    PayoutApprove(String),
    /// Create promo code: /promocreate code | percent|fixed | value | [max_uses] | [min_purchase] | [days].
    PromoCreate(String),
    /// List promo codes (seller: own; admin: all). /promolist
    PromoList,
    /// Revoke promo code. /promodel code
    PromoDel(String),
    /// Apply/validate promo code at checkout. /promo code
    Promo(String),
    /// List customers (seller: own; admin: all). /customers
    Customers,
    /// View customer: /customer <id>
    Customer(String),
    /// Add note to customer: /customernote <id> | <text>
    CustomerNote(String),
    /// Tag customer: /customertag <id> | <tag>
    CustomerTag(String),
    /// Block customer: /blockcustomer <id>
    BlockCustomer(String),
    /// Open support ticket (buyer): /support <text>
    Support(String),
    /// List tickets (seller). /tickets
    Tickets,
    /// Reply to ticket: /reply <id> | <text>
    Reply(String),
    /// Close ticket: /closeticket <id>
    CloseTicket(String),
    /// Escalate ticket: /escalate <id>
    Escalate(String),
    /// Show referral code + stats + earnings. /referral
    Referral,
    /// Apply referral code: /redeemref <code>.
    RedeemRef(String),
    /// Show bonus balance. /balance
    Balance,
    /// Apply bonus at checkout: /redeembonus <amount>.
    RedeemBonus(String),
    /// Leave a review: /review <product> <rating> [text].
    Review(String),
    /// List reviews for a product. /reviews <product>
    Reviews(String),
    /// Set a reorder reminder: /reorder <product> <days>.
    Reorder(String),
    /// Disable reorder reminder. /reorderoff <product>
    ReorderOff(String),
}

#[tracing::instrument(skip(
    _bot,
    msg,
    dialogue,
    db,
    cfg,
    bot_ctx,
    bot_manager,
    crypto,
    ai,
    reg,
    cmd
))]
pub async fn cmd_handler(
    _bot: Bot,
    msg: Message,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    db: Arc<Db>,
    cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
    bot_manager: Arc<BotManager>,
    owner_id: i64,
    crypto: Option<Arc<CryptoPay>>,
    ai: Option<Arc<crate::ai::AiService>>,
    reg: Arc<CommandRegistry>,
    cmd: Command,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    // register user for public mode / broadcasting
    if user_id != 0 {
        let uname = msg.from.as_ref().and_then(|u| u.username.clone());
        db.upsert_user(user_id, uname.as_deref());
        // NOTE: sellers are NOT auto-created here. Seller status must come from
        // an explicit /becomeseller or /setup flow (or the admin API) so the
        // seller role — and the channels/privileges it unlocks — is always
        // opt-in.
    }
    let role = roles::role_of(&db, user_id, bot_ctx.owner_id);
    let is_admin = role >= roles::Role::Admin;
    let is_seller = role >= roles::Role::Seller;
    let user_cmd = matches!(
        cmd,
        Command::Start
            | Command::Help
            | Command::Menu
            | Command::Plans
            | Command::Shop
            | Command::Buy(_)
            | Command::Paid(_)
            | Command::Redeem(_)
            | Command::Cart
            | Command::CartAdd(_)
            | Command::CartRemove(_)
            | Command::CartClear
            | Command::Checkout
            | Command::Services
            | Command::Book(_)
            | Command::Bookings
            | Command::MyBookings
            | Command::CancelBooking(_)
            | Command::Setup(_)
            | Command::BecomeSeller
            | Command::Promo(_)
            | Command::Referral
            | Command::RedeemRef(_)
            | Command::Balance
            | Command::RedeemBonus(_)
            | Command::Support(_)
            | Command::Review(_)
            | Command::Reviews(_)
            | Command::Reorder(_)
            | Command::ReorderOff(_)
    );
    let seller_cmd = matches!(
        cmd,
        Command::Channel
            | Command::Channels
            | Command::SetChannel(_)
            | Command::Post(_)
            | Command::Schedule(_)
            | Command::PostPhoto(_)
            | Command::Edit(_)
            | Command::Delete(_)
            | Command::Pin(_)
            | Command::Unpin(_)
            | Command::UnpinMsg(_)
            | Command::Invite(_)
            | Command::SetDesc(_)
            | Command::SetTitle(_)
            | Command::Info(_)
            | Command::Crypto(_)
            | Command::PlanAdd(_)
            | Command::PlanDel(_)
            | Command::PromoCreate(_)
            | Command::PromoList
            | Command::PromoDel(_)
            | Command::Customers
            | Command::Customer(_)
            | Command::CustomerNote(_)
            | Command::CustomerTag(_)
            | Command::BlockCustomer(_)
            | Command::Tickets
            | Command::Reply(_)
            | Command::CloseTicket(_)
            | Command::Escalate(_)
            | Command::ProductAdd(_)
            | Command::Products
            | Command::ProductDel(_)
            | Command::Orders
            | Command::OrderSet(_)
            | Command::Grant(_)
            | Command::Revoke(_)
            | Command::Subs
            | Command::ServiceAdd(_)
            | Command::SlotAdd(_)
            | Command::Bookings
            | Command::BookingConfirm(_)
            | Command::BookingDecline(_)
            | Command::ExpireCheck
            | Command::Broadcast(_)
            | Command::Ai(_)
            | Command::AiPost(_)
            | Command::GenCodes(_)
            | Command::Codes
            | Command::SetWebPassword(_)
            | Command::McpToken(_)
            | Command::Dashboard
    );
    // Admin-tier commands: admin/owner only (sellers and plain users are
    // excluded here; each handler keeps its own `!is_admin` guard like
    // /broadcast and /ai). /sellers lives at this tier, matching its
    // in-handler check and the documented admin access.
    let admin_cmd = matches!(
        cmd,
        Command::Sellers | Command::RefundApprove(_) | Command::PayoutApprove(_)
    );
    let owner_cmd = matches!(
        cmd,
        Command::AddAdmin(_)
            | Command::RemoveAdmin(_)
            | Command::Admins
            | Command::WebUsers
            | Command::ExportData(_)
            | Command::DeleteData(_)
    );
    let allowed = user_cmd
        || (is_seller && seller_cmd)
        || (is_admin && seller_cmd)
        || (is_admin && admin_cmd)
        || (role == roles::Role::Owner && owner_cmd);
    if !allowed {
        // silent ignore for unauthorized commands
        return Ok(());
    }

    let chat_id = msg.chat.id;
    match cmd {
        Command::Help => {
            // Role-aware first touch (buyer review P3#15): plain buyers get
            // the shopping surface first, then the full list; sellers/admins
            // keep the plain listing.
            let mut help = String::new();
            if role == roles::Role::User {
                help.push_str(
                    "For buyers:\n\
                     /shop — browse the catalogue\n\
                     /plans — list subscription plans\n\
                     /buy <name> — buy a plan or product\n\
                     /paid <invoice_id> — check payment status\n\
                     /redeem <code> — redeem an activation code\n\
                     /services — list bookable services\n\
                     /book <service> — book a service\n\
                     /mybookings — your bookings\n\
                     /cancelbooking <id> — cancel a booking\n\n\
                     All commands:\n",
                );
            }
            help.push_str(&Command::descriptions().to_string());
            help.push_str(&reg.descriptions());
            bot.send_message(chat_id, help).await?;
        }
        Command::Start => {
            if is_seller || is_admin {
                bot.send_message(chat_id, "Telegram Agent (owner-only). Use /menu or /help.")
                    .await?;
            } else {
                // Buyer first touch (P2#10): the shop, not the operator console.
                bot.send_message(
                    chat_id,
                    "Welcome! This is a Telegram shop. Browse /shop and /plans, buy with /buy <name>, redeem a code with /redeem, see /help.",
                )
                .reply_markup(buyer_menu_kb())
                .await?;
                return Ok(());
            }
            send_menu(&bot, chat_id, role).await?;
        }
        Command::Menu => {
            send_menu(&bot, chat_id, role).await?;
        }
        Command::Channel => {
            let ch = channel_of(&db, &cfg, Some(user_id), is_admin);
            bot.send_message(chat_id, format!("Default target channel: {}", ch))
                .await?;
        }
        Command::Channels => {
            let mut set = std::collections::BTreeSet::new();
            if let Some(c) = db.get_config("channel") {
                set.insert(c);
            }
            for p in db.list_plans(None) {
                if let Some(c) = p.4 {
                    set.insert(c);
                }
            }
            if set.is_empty() {
                bot.send_message(chat_id, "No channels configured.").await?;
            } else {
                bot.send_message(
                    chat_id,
                    format!(
                        "Known channels:\n{}",
                        set.iter()
                            .map(|c| format!("- {}", c))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ),
                )
                .await?;
            }
        }
        Command::SetChannel(c) => {
            let c = c.trim().to_string();
            if c.is_empty() {
                bot.send_message(chat_id, "Usage: /setchannel @channel or id")
                    .await?;
            } else {
                // Proof of control (L10 BLOCK-3) for non-admin senders: they
                // may only bind a channel they actually control (admin of the
                // chat; the bot a member). Admins/owner keep the unvalidated
                // bootstrap path.
                if !is_admin && let Some(m) = check_seller_channel_target(&bot, &c, user_id).await {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
                db.set_seller_channel(user_id, &c);
                bot.send_message(chat_id, format!("Target channel set to: {}", c))
                    .await?;
            }
        }
        Command::Post(text) => {
            let (chan, rest) = split_channel_arg(&text);
            let text = rest.trim().to_string();
            if text.is_empty() {
                bot.send_message(chat_id, "Usage: /post [@channel] <text>")
                    .await?;
            } else {
                let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                    Ok(ch) => ch,
                    Err(m) => {
                        bot.send_message(chat_id, m).await?;
                        return Ok(());
                    }
                };
                // L10 outbound content gate BEFORE transport. This is a raw
                // `Bot::send_message` path (non-ContentSender); the acting
                // seller is the best available scope, so seller rules apply
                // alongside the non-overridable global floor.
                if crate::handlers::moderation::gate_outbound(&db, user_id, Some(bot_ctx.id), &text)
                    .is_err()
                {
                    // Held: no transport, no log_post. The gate already wrote
                    // an opaque audit row and a scoped warn log; no body or
                    // username is logged here.
                    return Ok(());
                }
                match bot.send_message(util::to_chat_id(&ch), text.clone()).await {
                    Ok(sent) => {
                        crate::metrics::bot_sent();
                        let _ = db.log_post(user_id, &ch, sent.id.0, "text", Some(&text), "bot");
                        bot.send_message(
                            chat_id,
                            format!("Posted to {} (message_id={}).", ch, sent.id.0),
                        )
                        .await?;
                    }
                    Err(e) => {
                        bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                    }
                }
            }
        }
        Command::Schedule(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            // Tenancy gate first: an Err here must not create a scheduled
            // post pointing at another seller's channel.
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let parts: Vec<&str> = rest.splitn(2, '|').collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /schedule [@channel] <datetime> | <text>")
                    .await?;
            } else {
                let dt_str = parts[0].trim();
                let text = parts[1].trim().to_string();
                if text.is_empty() {
                    bot.send_message(chat_id, "Usage: /schedule [@channel] <datetime> | <text>")
                        .await?;
                } else {
                    let scheduled_at =
                        parse_schedule_datetime(dt_str).unwrap_or_else(|| dt_str.to_string());
                    let id = db.create_scheduled_post(user_id, &ch, &text, "text", &scheduled_at);
                    bot.send_message(
                        chat_id,
                        format!("Scheduled post #{} for {}.", id, scheduled_at),
                    )
                    .await?;
                }
            }
        }
        Command::PostPhoto(cap) => {
            let (chan, rest) = split_channel_arg(&cap);
            let cap = rest.trim().to_string();
            // Tenancy gate before arming the dialogue: a rejected channel
            // must not be written into the global pending_channel that the
            // photo handler (messages.rs) reads for admins.
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            db.set_config("pending_channel", &ch);
            dialogue.update(State::AwaitingPhoto).await?;
            if cap.is_empty() {
                db.set_config("pending_caption", "");
                bot.send_message(chat_id, format!("Send the photo now (target: {}).", ch))
                    .await?;
            } else {
                db.set_config("pending_caption", &cap);
                bot.send_message(chat_id, format!("Send the photo now (target: {}).", ch))
                    .await?;
            }
        }
        Command::Edit(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let parts: Vec<&str> = rest.splitn(2, '|').collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /edit [@channel] <msg_id> | <new text>")
                    .await?;
            } else {
                let mid: i32 = match parts[0].trim().parse() {
                    Ok(v) => v,
                    Err(_) => {
                        bot.send_message(chat_id, "Invalid message id").await?;
                        return Ok(());
                    }
                };
                let newtext = parts[1].trim();
                match bot
                    .edit_message_text(util::to_chat_id(&ch), MessageId(mid), newtext)
                    .await
                {
                    Ok(_) => {
                        bot.send_message(chat_id, "Edited.").await?;
                    }
                    Err(e) => {
                        bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                    }
                }
            }
        }
        Command::Delete(arg) => {
            // arg may be "@chan 123" or "123"
            let argstr = arg.clone();
            let (chan, rest) = split_channel_arg(&argstr);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let id: i64 = rest.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /delete [@channel] <msg_id>")
                    .await?;
                return Ok(());
            }
            match bot
                .delete_message(util::to_chat_id(&ch), MessageId(id as i32))
                .await
            {
                Ok(_) => {
                    bot.send_message(chat_id, "Deleted.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::Pin(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let id: i64 = rest.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /pin [@channel] <msg_id>")
                    .await?;
                return Ok(());
            }
            match bot
                .pin_chat_message(util::to_chat_id(&ch), MessageId(id as i32))
                .await
            {
                Ok(_) => {
                    bot.send_message(chat_id, "Pinned.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::Unpin(arg) => {
            let (chan, _rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            match bot.unpin_all_chat_messages(util::to_chat_id(&ch)).await {
                Ok(_) => {
                    bot.send_message(chat_id, "Unpinned all.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::UnpinMsg(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let id: i64 = rest.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /unpinmsg [@channel] <msg_id>")
                    .await?;
                return Ok(());
            }
            match bot
                .unpin_chat_message(util::to_chat_id(&ch))
                .message_id(MessageId(id as i32))
                .await
            {
                Ok(_) => {
                    bot.send_message(chat_id, "Unpinned.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::Invite(arg) => {
            let (chan, _) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            match bot.create_chat_invite_link(util::to_chat_id(&ch)).await {
                Ok(link) => {
                    bot.send_message(chat_id, format!("Invite: {}", link.invite_link))
                        .await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::SetDesc(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let text = rest.trim().to_string();
            if text.is_empty() {
                bot.send_message(chat_id, "Usage: /setdesc [@channel] <text>")
                    .await?;
                return Ok(());
            }
            match bot
                .set_chat_description(util::to_chat_id(&ch))
                .description(text.clone())
                .await
            {
                Ok(_) => {
                    bot.send_message(chat_id, "Description updated.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::SetTitle(arg) => {
            let (chan, rest) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            let text = rest.trim().to_string();
            if text.is_empty() {
                bot.send_message(chat_id, "Usage: /settitle [@channel] <text>")
                    .await?;
                return Ok(());
            }
            match bot
                .set_chat_title(util::to_chat_id(&ch), text.clone())
                .await
            {
                Ok(_) => {
                    bot.send_message(chat_id, "Title updated.").await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::Info(arg) => {
            let (chan, _) = split_channel_arg(&arg);
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            match bot.get_chat(util::to_chat_id(&ch)).await {
                Ok(c) => {
                    let title = c.title().unwrap_or("(no title)");
                    let desc = c.description().unwrap_or("");
                    let members = bot.get_chat_member_count(util::to_chat_id(&ch)).await.ok();
                    bot.send_message(
                        chat_id,
                        format!(
                            "Title: {}\nDescription: {}\nMembers: {:?}",
                            title, desc, members
                        ),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                }
            }
        }
        Command::Crypto(arg) => {
            handle_crypto(&bot, chat_id, &db, &cfg, crypto.clone(), user_id, &arg).await?;
        }
        Command::SetupBot => {
            setup_bot_profile(&bot).await?;
            bot.send_message(chat_id, "Bot profile/commands configured.")
                .await?;
        }
        Command::Plans => {
            // Seller-scoped: sellers see their own catalogue (incl. inactive),
            // admins keep the global view, plain users see the active-only
            // shop lines across sellers. The seller_id=0 global namespace is
            // admin-managed and never advertised to buyers (P2#12) — a buyer
            // could never resolve it through /buy. Buyer lines carry the
            // seller identifier (P2#11) so /buy <seller>/<name> is
            // constructible straight from the listing.
            if role == roles::Role::Seller {
                let plans = db.list_plans(Some(user_id));
                if plans.is_empty() {
                    bot.send_message(
                        chat_id,
                        "No plans configured. Add one with /planadd name | price | asset | days.",
                    )
                    .await?;
                } else {
                    bot.send_message(chat_id, plans_text(plans)).await?;
                }
            } else if is_admin {
                let plans = db.list_plans(None);
                if plans.is_empty() {
                    bot.send_message(chat_id, "No plans configured.").await?;
                } else {
                    bot.send_message(chat_id, plans_text(plans)).await?;
                }
            } else {
                let plans: Vec<crate::db::PlanRow> = db
                    .list_plans(None)
                    .into_iter()
                    .filter(|p| p.5 && p.6 != 0)
                    .collect();
                if plans.is_empty() {
                    bot.send_message(chat_id, "No plans yet — check back soon.")
                        .await?;
                } else {
                    let mut out = String::from("Plans:\n");
                    for (name, price, asset, days, _ch, _active, seller) in plans {
                        out.push_str(&format!(
                            "- {} : {} {} / {} days — by {}\n",
                            name,
                            price,
                            asset,
                            days,
                            seller_display(&db, seller)
                        ));
                    }
                    bot.send_message(chat_id, out).await?;
                }
            }
        }
        Command::PlanAdd(arg) => match parse_plan_add(&arg) {
            Err(PlanAddError::Usage) => {
                bot.send_message(chat_id, "Usage: /planadd name | price | asset | days")
                    .await?;
            }
            Err(PlanAddError::Days(raw)) => {
                bot.send_message(
                    chat_id,
                    format!("Invalid days '{}'. Must be 1..={}.", raw, MAX_PLAN_DAYS),
                )
                .await?;
            }
            Ok((name, price, asset, days)) => {
                let ch = channel_of(&db, &cfg, Some(user_id), is_admin);
                db.add_plan(user_id, name, price, asset, days, Some(&ch));
                bot.send_message(chat_id, format!("Plan '{}' added.", name))
                    .await?;
            }
        },
        Command::PlanDel(name) => {
            db.remove_plan(user_id, name.trim());
            bot.send_message(chat_id, "Plan removed.").await?;
        }
        Command::PromoCreate(arg) => {
            let parts: Vec<&str> = arg.split('|').map(|s| s.trim()).collect();
            if parts.len() < 3 || parts[0].is_empty() {
                bot.send_message(
                    chat_id,
                    "Usage: /promocreate <code> | <percent|fixed> | <value> | [max_uses] | [min_purchase] | [days]",
                )
                .await?;
                return Ok(());
            }
            let code = parts[0];
            let promo_type = parts[1].to_lowercase();
            if promo_type != "percent" && promo_type != "fixed" {
                bot.send_message(chat_id, "Type must be 'percent' or 'fixed'.")
                    .await?;
                return Ok(());
            }
            let value: f64 = match parts[2].parse() {
                Ok(v) => v,
                Err(_) => {
                    bot.send_message(chat_id, "Invalid value.").await?;
                    return Ok(());
                }
            };
            if value <= 0.0 {
                bot.send_message(chat_id, "Value must be > 0.").await?;
                return Ok(());
            }
            if promo_type == "percent" && value > 100.0 {
                bot.send_message(chat_id, "Percent value must be <= 100.")
                    .await?;
                return Ok(());
            }
            let max_uses = parts.get(3).and_then(|s| {
                let s = s.trim();
                if s.is_empty() || s == "-" {
                    None
                } else {
                    s.parse::<i64>().ok()
                }
            });
            let min_purchase = parts
                .get(4)
                .and_then(|s| {
                    let s = s.trim();
                    if s.is_empty() || s == "-" {
                        None
                    } else {
                        s.parse::<f64>().ok()
                    }
                })
                .unwrap_or(0.0);
            let days = parts.get(5).and_then(|s| s.trim().parse::<i64>().ok());
            let expires_at = days.map(|d| (Utc::now() + Duration::days(d)).to_rfc3339());
            let seller_id = user_id;
            match db.create_promo_code(
                seller_id,
                code,
                &promo_type,
                value,
                None,
                max_uses,
                min_purchase,
                "all",
                None,
                expires_at.as_deref(),
            ) {
                Ok(()) => {
                    bot.send_message(
                        chat_id,
                        format!("Promo code '{}' created ({} {}).", code, promo_type, value),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Error: {}", e)).await?;
                }
            }
        }
        Command::PromoList => {
            let promos = if is_admin {
                db.list_all_promo_codes()
            } else {
                db.list_promo_codes(user_id)
            };
            if promos.is_empty() {
                bot.send_message(chat_id, "No promo codes.").await?;
            } else {
                let mut out = String::from(
                    "Promo codes (code | type | value | uses/max | min | expires | state):\n",
                );
                for (
                    code,
                    sid,
                    promo_type,
                    value,
                    _currency,
                    max_uses,
                    uses,
                    min_purchase,
                    _scope,
                    _scope_id,
                    expires_at,
                    active,
                    _created,
                ) in promos
                {
                    out.push_str(&format!(
                        "{} (seller {}) | {} | {} | {}/{} | min {} | {} | {}\n",
                        code,
                        sid,
                        promo_type,
                        value,
                        uses,
                        max_uses
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "∞".to_string()),
                        min_purchase,
                        expires_at.unwrap_or_else(|| "-".to_string()),
                        if active != 0 { "active" } else { "revoked" }
                    ));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::PromoDel(code) => {
            let code = code.trim();
            if code.is_empty() {
                bot.send_message(chat_id, "Usage: /promodel <code>").await?;
                return Ok(());
            }
            let ok = if is_admin {
                db.revoke_promo_code_all(code)
            } else {
                db.revoke_promo_code(user_id, code)
            };
            if ok {
                bot.send_message(chat_id, "Promo code revoked.").await?;
            } else {
                bot.send_message(chat_id, "Promo code not found.").await?;
            }
        }
        Command::Promo(code) => {
            let code = code.trim();
            if code.is_empty() {
                bot.send_message(chat_id, "Usage: /promo <code>").await?;
                return Ok(());
            }
            let promos = db.list_all_promo_codes();
            let found = promos.into_iter().find(|p| p.0 == code && p.11 != 0);
            match found {
                Some(p) => {
                    // p: code, seller, type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires, active, created
                    let expired =
                        p.10.as_ref()
                            .map(|e| {
                                chrono::DateTime::parse_from_rfc3339(e)
                                    .map(|dt| dt < chrono::Utc::now())
                                    .unwrap_or(false)
                            })
                            .unwrap_or(false);
                    if expired {
                        bot.send_message(chat_id, "Promo code expired.").await?;
                        return Ok(());
                    }
                    if let Some(cap) = p.5
                        && p.6 >= cap
                    {
                        bot.send_message(chat_id, "Promo code usage cap reached.")
                            .await?;
                        return Ok(());
                    }
                    let discount_desc = match p.2.as_str() {
                        "percent" => format!("{}% off", p.3),
                        "fixed" => format!("{} off", p.3),
                        _ => p.2.clone(),
                    };
                    bot.send_message(
                        chat_id,
                        format!(
                            "Promo code '{}' valid: {} (min purchase {}). Apply at checkout with /buy; seller {}.",
                            code,
                            discount_desc,
                            p.7,
                            p.1
                        ),
                    )
                    .await?;
                }
                None => {
                    bot.send_message(chat_id, "Invalid or inactive promo code.")
                        .await?;
                }
            }
        }
        Command::Referral => {
            let seller_id = if is_seller { user_id } else { 0 };
            let code = db.create_referral_code(seller_id, user_id);
            let refs = db.list_referrals(seller_id, Some(user_id));
            let total = refs.len();
            let rewarded = refs.iter().filter(|r| r.4 == "rewarded").count();
            let earnings: f64 = refs.iter().map(|r| r.5).sum();
            let bal = db.bonus_balance(seller_id, user_id);
            bot.send_message(
                chat_id,
                format!(
                    "Your referral code: {}\nReferred: {} (rewarded: {})\nEarnings: {}\nBonus balance (minor): {}",
                    code, total, rewarded, earnings, bal
                ),
            )
            .await?;
        }
        Command::RedeemRef(code) => {
            let code = code.trim();
            if code.is_empty() {
                bot.send_message(chat_id, "Usage: /redeemref <code>")
                    .await?;
                return Ok(());
            }
            // Resolve seller scope from the code.
            let mut found: Option<(i64, i64)> = None;
            let mut scopes = db.list_sellers();
            scopes.push(0);
            for sid in scopes {
                if let Some(rid) = db.get_referrer_by_code(sid, code) {
                    found = Some((sid, rid));
                    break;
                }
            }
            match found {
                None => {
                    bot.send_message(chat_id, "Invalid referral code.").await?;
                }
                Some((sid, rid)) => {
                    if rid == user_id {
                        bot.send_message(chat_id, "Self-referral is not allowed.")
                            .await?;
                        return Ok(());
                    }
                    match db.create_referral(sid, rid, user_id, code) {
                        Ok(()) => {
                            bot.send_message(
                                chat_id,
                                "Referral registered. Reward will be credited on first paid purchase.",
                            )
                            .await?;
                        }
                        Err(e) => {
                            bot.send_message(chat_id, format!("Could not redeem referral: {}", e))
                                .await?;
                        }
                    }
                }
            }
        }
        Command::Balance => {
            // Show balances across known scopes.
            let mut out = String::from("Bonus balances (minor units):\n");
            let mut scopes = db.list_sellers();
            scopes.push(0);
            let mut any = false;
            for sid in scopes {
                let bal = db.bonus_balance(sid, user_id);
                if bal != 0 {
                    any = true;
                    out.push_str(&format!("seller {}: {}\n", sid, bal));
                }
            }
            if !any {
                out.push_str("No bonus balance yet.");
            }
            bot.send_message(chat_id, out).await?;
        }
        Command::RedeemBonus(amount) => {
            let amount: i64 = match amount.trim().parse() {
                Ok(v) if v > 0 => v,
                _ => {
                    bot.send_message(chat_id, "Usage: /redeembonus <amount (minor units)>")
                        .await?;
                    return Ok(());
                }
            };
            // Debit from the first scope that has enough balance.
            let mut scopes = db.list_sellers();
            scopes.push(0);
            let mut done = false;
            for sid in scopes {
                if db.bonus_balance(sid, user_id) >= amount {
                    match db.bonus_debit(sid, user_id, amount, "redeembonus") {
                        Ok(newbal) => {
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Redeemed {} bonus. New balance: {} (seller {}).",
                                    amount, newbal, sid
                                ),
                            )
                            .await?;
                            done = true;
                            break;
                        }
                        Err(e) => {
                            bot.send_message(chat_id, format!("Error: {}", e)).await?;
                            done = true;
                            break;
                        }
                    }
                }
            }
            if !done {
                bot.send_message(chat_id, "Insufficient bonus balance.")
                    .await?;
            }
        }
        Command::Customers => {
            let seller_ids: Vec<i64> = if is_admin {
                let mut v = db.list_sellers();
                v.push(0);
                v
            } else {
                vec![user_id]
            };
            let mut out = String::from("Customers:\n");
            let mut n = 0i64;
            for sid in seller_ids {
                for (uid, last_seen, tags, spent, orders, status) in
                    db.list_customers(sid, None, None)
                {
                    out.push_str(&format!(
                        "- {} (seller {}) | last {} | tags:{} | spent:{} orders:{} | {}\n",
                        uid,
                        sid,
                        last_seen,
                        tags.unwrap_or_default(),
                        spent,
                        orders,
                        status
                    ));
                    n += 1;
                }
            }
            if n == 0 {
                bot.send_message(chat_id, "No customers.").await?;
            } else {
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::Customer(arg) => {
            let id: i64 = arg.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /customer <user_id>")
                    .await?;
                return Ok(());
            }
            let sellers: Vec<i64> = if is_admin {
                let mut v = db.list_sellers();
                v.push(0);
                v
            } else {
                vec![user_id]
            };
            let mut found = false;
            for sid in sellers {
                if let Some(c) = db.get_customer(sid, id) {
                    found = true;
                    let (_u, _s, first, last, tags, notes, spent, orders, status) = c;
                    bot.send_message(
                        chat_id,
                        format!(
                            "Customer {} (seller {})\nfirst:{} last:{}\ntags:{}\nnotes:{}\nspent:{} orders:{}\nstatus:{}",
                            id,
                            sid,
                            first,
                            last,
                            tags.unwrap_or_default(),
                            notes.unwrap_or_default(),
                            spent,
                            orders,
                            status
                        ),
                    )
                    .await?;
                }
            }
            if !found {
                bot.send_message(chat_id, "Customer not found.").await?;
            }
        }
        Command::CustomerNote(arg) => {
            let parts: Vec<&str> = arg.splitn(2, '|').collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /customernote <id> | <text>")
                    .await?;
                return Ok(());
            }
            let id: i64 = parts[0].trim().parse().unwrap_or(0);
            let text = parts[1].trim();
            if id == 0 || text.is_empty() {
                bot.send_message(chat_id, "Usage: /customernote <id> | <text>")
                    .await?;
                return Ok(());
            }
            let sellers: Vec<i64> = if is_admin {
                let mut v = db.list_sellers();
                v.push(0);
                v
            } else {
                vec![user_id]
            };
            let mut ok = false;
            for sid in sellers {
                if db.get_customer(sid, id).is_some() && db.update_customer_notes(sid, id, text) {
                    ok = true;
                }
            }
            if ok {
                bot.send_message(chat_id, "Note updated.").await?;
            } else {
                bot.send_message(chat_id, "Customer not found.").await?;
            }
        }
        Command::CustomerTag(arg) => {
            let parts: Vec<&str> = arg.splitn(2, '|').collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /customertag <id> | <tag>")
                    .await?;
                return Ok(());
            }
            let id: i64 = parts[0].trim().parse().unwrap_or(0);
            let tag = parts[1].trim();
            if id == 0 || tag.is_empty() {
                bot.send_message(chat_id, "Usage: /customertag <id> | <tag>")
                    .await?;
                return Ok(());
            }
            let sellers: Vec<i64> = if is_admin {
                let mut v = db.list_sellers();
                v.push(0);
                v
            } else {
                vec![user_id]
            };
            let mut ok = false;
            for sid in sellers {
                if let Some(c) = db.get_customer(sid, id) {
                    let mut tags: Vec<String> =
                        c.4.clone()
                            .unwrap_or_default()
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    if !tags.iter().any(|t| t == tag) {
                        tags.push(tag.to_string());
                    }
                    if db.update_customer_tags(sid, id, &tags.join(",")) {
                        ok = true;
                    }
                }
            }
            if ok {
                bot.send_message(chat_id, "Tag updated.").await?;
            } else {
                bot.send_message(chat_id, "Customer not found.").await?;
            }
        }
        Command::BlockCustomer(arg) => {
            let id: i64 = arg.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /blockcustomer <user_id>")
                    .await?;
                return Ok(());
            }
            let sellers: Vec<i64> = if is_admin {
                let mut v = db.list_sellers();
                v.push(0);
                v
            } else {
                vec![user_id]
            };
            let mut ok = false;
            for sid in sellers {
                if db.get_customer(sid, id).is_some() && db.set_customer_status(sid, id, "blocked")
                {
                    ok = true;
                }
            }
            if ok {
                bot.send_message(chat_id, "Customer blocked.").await?;
            } else {
                bot.send_message(chat_id, "Customer not found.").await?;
            }
        }
        Command::Support(text) => {
            let text = text.trim();
            if text.is_empty() {
                bot.send_message(chat_id, "Usage: /support <text>").await?;
                return Ok(());
            }
            let ticket_id = db.create_ticket(0, user_id, text);
            db.add_ticket_message(ticket_id, "buyer", user_id, text);
            bot.send_message(chat_id, format!("Ticket #{} created.", ticket_id))
                .await?;
        }
        Command::Review(arg) => {
            let arg = arg.trim();
            if arg.is_empty() {
                bot.send_message(chat_id, "Usage: /review <product> <rating> [text]")
                    .await?;
                return Ok(());
            }
            // Find the rating token (1..=5) to support multi-word product names.
            let tokens: Vec<&str> = arg.split_whitespace().collect();
            let mut rating_idx: Option<usize> = None;
            for (i, t) in tokens.iter().enumerate() {
                if let Ok(r) = t.parse::<i64>()
                    && (1..=5).contains(&r)
                {
                    rating_idx = Some(i);
                    break;
                }
            }
            let (product, rating, text) = match rating_idx {
                Some(i) if i > 0 => {
                    let product = tokens[..i].join(" ");
                    let rating: i64 = tokens[i].parse().unwrap_or(0);
                    let text = if i + 1 < tokens.len() {
                        Some(tokens[i + 1..].join(" "))
                    } else {
                        None
                    };
                    (product, rating, text)
                }
                _ => {
                    bot.send_message(chat_id, "Usage: /review <product> <rating> [text]")
                        .await?;
                    return Ok(());
                }
            };
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, &product);
            let _ = plans;
            if products.len() != 1 {
                if products.is_empty() {
                    bot.send_message(chat_id, "Product not found.").await?;
                } else {
                    bot.send_message(chat_id, "Multiple matches. Be more specific.")
                        .await?;
                }
                return Ok(());
            }
            let p = products.into_iter().next().unwrap();
            let (pid, seller_id, pname, ..) = p;
            let _ = pname;
            match db.create_review(user_id, seller_id, pid, rating, text.as_deref()) {
                Ok(id) => {
                    bot.send_message(
                        chat_id,
                        format!("Review #{} submitted (pending approval).", id),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Error: {}", e)).await?;
                }
            }
        }
        Command::Reviews(arg) => {
            let name = arg.trim();
            if name.is_empty() {
                bot.send_message(chat_id, "Usage: /reviews <product>")
                    .await?;
                return Ok(());
            }
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, name);
            let _ = plans;
            if products.len() != 1 {
                if products.is_empty() {
                    bot.send_message(chat_id, "Product not found.").await?;
                } else {
                    bot.send_message(chat_id, "Multiple matches. Be more specific.")
                        .await?;
                }
                return Ok(());
            }
            let p = products.into_iter().next().unwrap();
            let (pid, seller_id, pname, ..) = p;
            let reviews = db.list_reviews(seller_id, pid, true);
            if reviews.is_empty() {
                bot.send_message(chat_id, format!("No reviews for '{}'.", pname))
                    .await?;
                return Ok(());
            }
            let mut out = format!(
                "Reviews for {} (avg {:?}):\n",
                pname,
                db.avg_rating(seller_id, pid)
            );
            for (_id, buyer, rating, text) in reviews {
                out.push_str(&format!("- {} (rating: {}) {}\n", buyer, rating, text));
            }
            bot.send_message(chat_id, out).await?;
        }
        Command::Reorder(arg) => {
            let arg = arg.trim();
            if arg.is_empty() {
                bot.send_message(chat_id, "Usage: /reorder <product> <days>")
                    .await?;
                return Ok(());
            }
            let tokens: Vec<&str> = arg.split_whitespace().collect();
            if tokens.len() < 2 {
                bot.send_message(chat_id, "Usage: /reorder <product> <days>")
                    .await?;
                return Ok(());
            }
            let days: i64 = match tokens.last().unwrap().parse() {
                Ok(d) if d > 0 => d,
                _ => {
                    bot.send_message(chat_id, "Days must be a positive number.")
                        .await?;
                    return Ok(());
                }
            };
            let product = tokens[..tokens.len() - 1].join(" ");
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, &product);
            let _ = plans;
            if products.len() != 1 {
                if products.is_empty() {
                    bot.send_message(chat_id, "Product not found.").await?;
                } else {
                    bot.send_message(chat_id, "Multiple matches. Be more specific.")
                        .await?;
                }
                return Ok(());
            }
            let p = products.into_iter().next().unwrap();
            let (pid, seller_id, pname, ..) = p;
            db.set_reorder_schedule(user_id, seller_id, pid, days);
            db.set_config(
                &format!("reorder_interval:{}:{}:{}", user_id, seller_id, pid),
                &days.to_string(),
            );
            bot.send_message(
                chat_id,
                format!("Reorder reminder set for '{}' every {} days.", pname, days),
            )
            .await?;
        }
        Command::ReorderOff(arg) => {
            let name = arg.trim();
            if name.is_empty() {
                bot.send_message(chat_id, "Usage: /reorderoff <product>")
                    .await?;
                return Ok(());
            }
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, name);
            let _ = plans;
            if products.len() != 1 {
                if products.is_empty() {
                    bot.send_message(chat_id, "Product not found.").await?;
                } else {
                    bot.send_message(chat_id, "Multiple matches. Be more specific.")
                        .await?;
                }
                return Ok(());
            }
            let p = products.into_iter().next().unwrap();
            let (pid, seller_id, ..) = p;
            if db.disable_reorder(user_id, seller_id, pid) {
                bot.send_message(chat_id, "Reorder reminder disabled.")
                    .await?;
            } else {
                bot.send_message(chat_id, "No active reorder reminder found.")
                    .await?;
            }
        }
        Command::Tickets => {
            let tickets = if is_admin {
                db.list_tickets(None, None, None)
            } else {
                db.list_tickets(Some(user_id), None, None)
            };
            if tickets.is_empty() {
                bot.send_message(chat_id, "No tickets.").await?;
            } else {
                let mut out = String::from("Tickets:\n");
                for (id, sid, buyer, subject, status, updated) in tickets {
                    out.push_str(&format!(
                        "#{} seller {} buyer {} | {} | {} | updated {}\n",
                        id, sid, buyer, subject, status, updated
                    ));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::Reply(arg) => {
            let parts: Vec<&str> = arg.splitn(2, '|').collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /reply <id> | <text>")
                    .await?;
                return Ok(());
            }
            let id: i64 = parts[0].trim().parse().unwrap_or(0);
            let text = parts[1].trim();
            if id == 0 || text.is_empty() {
                bot.send_message(chat_id, "Usage: /reply <id> | <text>")
                    .await?;
                return Ok(());
            }
            let ticket = match db.get_ticket(id) {
                Some(t) => t,
                None => {
                    bot.send_message(chat_id, "Ticket not found.").await?;
                    return Ok(());
                }
            };
            let (_id, seller_id, _buyer, _subj, _status, _c, _u) = ticket;
            if !is_admin && seller_id != user_id {
                bot.send_message(chat_id, "Ticket not found.").await?;
                return Ok(());
            }
            let role_str = if is_admin { "admin" } else { "seller" };
            db.add_ticket_message(id, role_str, user_id, text);
            bot.send_message(chat_id, format!("Replied to ticket #{}.", id))
                .await?;
        }
        Command::CloseTicket(arg) => {
            let id: i64 = arg.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /closeticket <id>")
                    .await?;
                return Ok(());
            }
            let ticket = match db.get_ticket(id) {
                Some(t) => t,
                None => {
                    bot.send_message(chat_id, "Ticket not found.").await?;
                    return Ok(());
                }
            };
            let (_id, seller_id, _buyer, _subj, _status, _c, _u) = ticket;
            if !is_admin && seller_id != user_id {
                bot.send_message(chat_id, "Ticket not found.").await?;
                return Ok(());
            }
            if db.set_ticket_status(id, "closed") {
                bot.send_message(chat_id, format!("Ticket #{} closed.", id))
                    .await?;
            } else {
                bot.send_message(chat_id, "Ticket not found.").await?;
            }
        }
        Command::Escalate(arg) => {
            let id: i64 = arg.trim().parse().unwrap_or(0);
            if id == 0 {
                bot.send_message(chat_id, "Usage: /escalate <id>").await?;
                return Ok(());
            }
            let ticket = match db.get_ticket(id) {
                Some(t) => t,
                None => {
                    bot.send_message(chat_id, "Ticket not found.").await?;
                    return Ok(());
                }
            };
            let (_id, seller_id, _buyer, _subj, _status, _c, _u) = ticket;
            if !is_admin && seller_id != user_id {
                bot.send_message(chat_id, "Ticket not found.").await?;
                return Ok(());
            }
            db.set_ticket_status(id, "escalated");
            db.add_ticket_message(id, "seller", user_id, "Escalated.");
            bot.send_message(chat_id, format!("Ticket #{} escalated.", id))
                .await?;
        }
        Command::ProductAdd(arg) => {
            // /productadd name | price | asset | kind | delivery | [stock]
            match parse_product_add(&arg) {
                Err(ProductAddError::Usage) => {
                    bot.send_message(
                        chat_id,
                        "Usage: /productadd name | price | asset | kind | delivery | [stock]\nkinds: digital_link, digital_file, physical; delivery '-' if none; stock '-' or omitted = unlimited",
                    )
                    .await?;
                }
                Err(ProductAddError::Kind(kind)) => {
                    bot.send_message(
                        chat_id,
                        format!(
                            "Invalid kind '{}'. Must be digital_link, digital_file or physical.",
                            kind
                        ),
                    )
                    .await?;
                }
                Err(ProductAddError::Delivery) => {
                    bot.send_message(
                        chat_id,
                        "Digital products require a delivery payload (link/text/file_id).",
                    )
                    .await?;
                }
                Err(ProductAddError::Stock(s)) => {
                    bot.send_message(
                        chat_id,
                        format!(
                            "Invalid stock '{}'. Use an integer, or '-' for unlimited.",
                            s
                        ),
                    )
                    .await?;
                }
                Ok((name, price, asset, kind, delivery, stock)) => {
                    match db.add_product(user_id, name, price, asset, kind, delivery, stock) {
                        Ok(id) => {
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Product '{}' added (#{}). Kind: {}. Stock: {}.",
                                    name,
                                    id,
                                    kind,
                                    stock
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| "unlimited".to_string())
                                ),
                            )
                            .await?;
                        }
                        Err(e) => {
                            // e.g. UNIQUE(seller_id, name) collision.
                            bot.send_message(chat_id, format!("Error: {}", e)).await?;
                        }
                    }
                }
            }
        }
        Command::Products => {
            let products = db.list_products(if is_admin { None } else { Some(user_id) });
            if products.is_empty() {
                bot.send_message(chat_id, "No products. Use /productadd.")
                    .await?;
            } else {
                let mut out = String::from("Products (name | price | kind | stock | state):\n");
                for (_id, _seller, name, price, asset, kind, _delivery, stock, active) in products {
                    out.push_str(&format!(
                        "- {} : {} {} | {} | {} | {}\n",
                        name,
                        price,
                        asset,
                        kind,
                        stock
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "unlimited".to_string()),
                        if active != 0 { "active" } else { "inactive" }
                    ));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::ProductDel(name) => {
            let name = name.trim();
            if db.get_product(user_id, name).is_none() {
                bot.send_message(chat_id, "Product not found.").await?;
            } else {
                db.set_product_active(user_id, name, false);
                bot.send_message(chat_id, "Product deactivated.").await?;
            }
        }
        Command::Orders => {
            // Seller-side fulfillment view: sellers see their own orders,
            // Admin/Owner see all. Buyers get their receipt by DM.
            let orders = db.list_orders(if is_admin { None } else { Some(user_id) });
            if orders.is_empty() {
                bot.send_message(chat_id, "No orders.").await?;
            } else {
                let mut out = String::from("Orders (order_id — status — buyer — tracking):\n");
                for (order_id, _seller, buyer, status, tracking, _created) in orders {
                    out.push_str(&format!(
                        "{} — {} — {} — {}\n",
                        order_id,
                        status,
                        buyer,
                        tracking.unwrap_or_else(|| "-".to_string())
                    ));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::OrderSet(arg) => {
            // /orderset <order_id> <status> [tracking...]: status always set;
            // tracking joined from the remaining words when present. Scopes to
            // the caller's own orders (set_order_status/set_order_tracking).
            match parse_order_set(&arg) {
                None => {
                    bot.send_message(chat_id, "Usage: /orderset <order_id> <status> [tracking]")
                        .await?;
                }
                Some((order_id, status, tracking)) => {
                    if !db.set_order_status(user_id, order_id, status) {
                        bot.send_message(
                            chat_id,
                            "Order not found or not yours. Status must be one of pending|shipped|delivered|refunded|cancelled.",
                        )
                        .await?;
                    } else if let Some(tracking) = tracking {
                        db.set_order_tracking(user_id, order_id, &tracking);
                        bot.send_message(
                            chat_id,
                            format!(
                                "Order {} updated: {} — tracking {}",
                                order_id, status, tracking
                            ),
                        )
                        .await?;
                    } else {
                        bot.send_message(
                            chat_id,
                            format!("Order {} updated: {}.", order_id, status),
                        )
                        .await?;
                    }
                }
            }
        }
        Command::Shop => {
            // Public catalogue: active products across all sellers, one line
            // each. Delivery payloads are never shown here — they ARE the
            // paid content. Buyers never see the seller_id=0 global namespace
            // (admin-managed, not resolvable through /buy); admin/owner keep
            // the full view (P2#12). Lines carry the seller identifier
            // (P2#11) so /buy <seller>/<name> is constructible.
            let mut products: Vec<crate::db::ProductRow> = db
                .list_products(None)
                .into_iter()
                .filter(|p| p.8 != 0)
                .collect();
            if role == roles::Role::User {
                products.retain(|p| p.1 != 0);
            }
            let mut out = String::from("Shop:\n");
            let mut n = 0;
            for (_id, seller, name, price, asset, kind, _delivery, _stock, _active) in products {
                out.push_str(&format!(
                    "- {} — {} {} ({}) — by {}\n",
                    name,
                    price,
                    asset,
                    match kind.as_str() {
                        "physical" => "product",
                        _ => "digital",
                    },
                    seller_display(&db, seller)
                ));
                n += 1;
            }
            if n == 0 {
                let empty = if is_seller {
                    "Shop is empty. List something with /productadd."
                } else {
                    "Shop is empty — check back soon."
                };
                bot.send_message(chat_id, empty).await?;
            } else {
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::Buy(name) => {
            // Multiplexed /buy: resolves the name across products AND plans.
            // Exactly one match buys it; zero matches or an ambiguous name
            // (>1 item, no seller disambiguation syntax in v1.2) errors out.
            let name = name.trim();
            if name.is_empty() {
                bot.send_message(chat_id, "Usage: /buy <name> (see /shop and /plans)")
                    .await?;
                return Ok(());
            }
            // Seller-scoped /buy resolution over the (seller_id, name) plan
            // namespace (2026-10-25 review HIGH). See resolve_buy_candidates
            // for the B5 fallback order.
            let (plans, mut products) = resolve_buy_candidates(&db, role, user_id, name);
            match (
                products.len() + plans.len(),
                products.pop(),
                plans.into_iter().next(),
            ) {
                (0, _, _) => {
                    bot.send_message(chat_id, "Not found. Use /shop or /plans.")
                        .await?;
                }
                (1, Some(product), _) => {
                    let (pid, p_seller, pname, price, asset, _kind, _delivery, stock, _active) =
                        product;
                    if stock == Some(0) {
                        bot.send_message(chat_id, format!("Product '{}' is out of stock.", pname))
                            .await?;
                        return Ok(());
                    }
                    let cp = crypto_or_load(&db, &cfg, crypto.clone());
                    match cp {
                        Some(c) => {
                            let desc = format!("Product: {}", pname);
                            match c
                                .create_invoice(&price, &asset, Some(&desc), Some("viewItem"), None)
                                .await
                            {
                                Ok(v) => {
                                    let inv_id = v
                                        .get("invoice_id")
                                        .map(|x| x.to_string())
                                        .unwrap_or_default();
                                    let pay_url =
                                        v.get("pay_url").and_then(|x| x.as_str()).unwrap_or("");
                                    if !inv_id.is_empty() {
                                        db.save_invoice(
                                            &inv_id,
                                            p_seller,
                                            &price,
                                            &asset,
                                            Some(&desc),
                                            "active",
                                            Some(pay_url),
                                            None,
                                            Some(user_id),
                                            Some(pid),
                                        );
                                    }
                                    send_invoice_created(
                                        &bot,
                                        chat_id,
                                        &format!("product '{}'", pname),
                                        &price,
                                        &asset,
                                        pay_url,
                                        &inv_id,
                                    )
                                    .await?;
                                }
                                Err(e) => {
                                    // Buyer-safe: raw provider errors never
                                    // reach the buyer (P1#9).
                                    tracing::warn!(
                                        "invoice creation failed for product '{}' (seller {}): {}",
                                        pname,
                                        p_seller,
                                        e
                                    );
                                    bot.send_message(chat_id, "Something went wrong creating your invoice. Please try again in a minute.")
                                        .await?;
                                }
                            }
                        }
                        None => {
                            // Buyer-safe: seller misconfiguration is never
                            // surfaced raw (P1#9).
                            tracing::warn!(
                                "invoice creation skipped: no crypto token configured (seller {}, product '{}')",
                                p_seller,
                                pname
                            );
                            bot.send_message(chat_id, "This shop can't take payments right now — the seller is finishing setup. Please contact the seller.")
                                .await?;
                        }
                    }
                }
                (1, None, Some(plan)) => {
                    // Plan tuple: (name, price, asset, days, channel, active, seller).
                    let (_pname, price, asset, _days, _ch, _active, plan_seller) = plan;
                    let cp = crypto_or_load(&db, &cfg, crypto.clone());
                    match cp {
                        Some(c) => {
                            let desc = format!("Plan: {}", name);
                            match c
                                .create_invoice(&price, &asset, Some(&desc), Some("viewItem"), None)
                                .await
                            {
                                Ok(v) => {
                                    let inv_id = v
                                        .get("invoice_id")
                                        .map(|x| x.to_string())
                                        .unwrap_or_default();
                                    let pay_url =
                                        v.get("pay_url").and_then(|x| x.as_str()).unwrap_or("");
                                    if !inv_id.is_empty() {
                                        db.save_invoice(
                                            &inv_id,
                                            plan_seller,
                                            &price,
                                            &asset,
                                            Some(&desc),
                                            "active",
                                            Some(pay_url),
                                            Some(name),
                                            Some(user_id),
                                            None,
                                        );
                                    }
                                    send_invoice_created(
                                        &bot,
                                        chat_id,
                                        &format!("plan '{}'", name),
                                        &price,
                                        &asset,
                                        pay_url,
                                        &inv_id,
                                    )
                                    .await?;
                                }
                                Err(e) => {
                                    // Buyer-safe: raw provider errors never
                                    // reach the buyer (P1#9).
                                    tracing::warn!(
                                        "invoice creation failed for plan '{}' (seller {}): {}",
                                        name,
                                        plan_seller,
                                        e
                                    );
                                    bot.send_message(chat_id, "Something went wrong creating your invoice. Please try again in a minute.")
                                        .await?;
                                }
                            }
                        }
                        None => {
                            // Buyer-safe: seller misconfiguration is never
                            // surfaced raw (P1#9).
                            tracing::warn!(
                                "invoice creation skipped: no crypto token configured (seller {}, plan '{}')",
                                plan_seller,
                                name
                            );
                            bot.send_message(chat_id, "This shop can't take payments right now — the seller is finishing setup. Please contact the seller.")
                                .await?;
                        }
                    }
                }
                _ => {
                    bot.send_message(
                        chat_id,
                        format!(
                            "Multiple sellers sell '{}'. Use /buy <seller>/<name> — see /shop for seller names.",
                            name
                        ),
                    )
                    .await?;
                }
            }
        }
        Command::Cart => {
            let items = db.cart_get_all_items(user_id);
            if items.is_empty() {
                bot.send_message(
                    chat_id,
                    "Your cart is empty. Add items with /cartadd <name>.",
                )
                .await?;
                return Ok(());
            }
            // Resolve each cart row to a display name + price.
            let mut lines = Vec::new();
            let mut total_by_asset: std::collections::BTreeMap<String, f64> =
                std::collections::BTreeMap::new();
            let mut ok = true;
            for (seller_id, _row_id, item_type, item_id, qty) in &items {
                let (name, price, asset) = if item_type == "plan" {
                    match db.get_plan_by_id(*item_id) {
                        Some((_, p, a, n, _, _)) => (n, p, a),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    // product
                    let prod = db
                        .list_products(Some(*seller_id))
                        .into_iter()
                        .find(|p| p.0 == *item_id);
                    match prod {
                        Some(p) => (p.2, p.3, p.4),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                };
                let q = *qty;
                let p: f64 = price.parse().unwrap_or(0.0);
                *total_by_asset.entry(asset.clone()).or_insert(0.0) += p * q as f64;
                lines.push(format!("- {} x{} @ {} {}", name, q, price, asset));
            }
            if !ok {
                bot.send_message(chat_id, "Cart contains an unknown item. Use /cartclear.")
                    .await?;
                return Ok(());
            }
            let mut out = String::from("Cart:\n");
            for l in lines {
                out.push_str(&l);
                out.push('\n');
            }
            for (asset, total) in &total_by_asset {
                out.push_str(&format!("Total: {:.2} {}\n", total, asset));
            }
            bot.send_message(chat_id, out).await?;
        }
        Command::CartAdd(arg) => {
            let arg = arg.trim();
            if arg.is_empty() {
                bot.send_message(chat_id, "Usage: /cartadd <name> [qty]")
                    .await?;
                return Ok(());
            }
            // Parse optional trailing qty.
            let (name, qty) = parse_cart_add_arg(arg);
            if qty <= 0 {
                bot.send_message(chat_id, "Quantity must be > 0.").await?;
                return Ok(());
            }
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, name);
            if plans.len() + products.len() != 1 {
                if plans.is_empty() && products.is_empty() {
                    bot.send_message(chat_id, "Not found. Use /shop or /plans.")
                        .await?;
                } else {
                    bot.send_message(chat_id, "Multiple matches. Be more specific.")
                        .await?;
                }
                return Ok(());
            }
            let (item_type, item_id, seller_id, stock) =
                if let Some(product) = products.into_iter().next() {
                    let (pid, pseller, pname, _price, _asset, _kind, _delivery, stock, _active) =
                        product;
                    let _ = pname;
                    ("product", pid, pseller, stock)
                } else if let Some(plan) = plans.into_iter().next() {
                    let (pname, _price, _asset, _days, _ch, _active, pseller) = plan;
                    // resolve plan id
                    let (pid, ..) = match db.get_plan(pseller, &pname) {
                        Some(x) => x,
                        None => {
                            bot.send_message(chat_id, "Plan not found.").await?;
                            return Ok(());
                        }
                    };
                    ("plan", pid, pseller, None)
                } else {
                    bot.send_message(chat_id, "Not found.").await?;
                    return Ok(());
                };
            if stock == Some(0) {
                bot.send_message(chat_id, "Item is out of stock.").await?;
                return Ok(());
            }
            // Enforce single-seller cart.
            let existing = db.cart_get_all_items(user_id);
            let existing_sellers: std::collections::BTreeSet<i64> =
                existing.iter().map(|i| i.0).collect();
            if !existing_sellers.is_empty() && !existing_sellers.contains(&seller_id) {
                bot.send_message(
                    chat_id,
                    "Cart contains items from another seller. /checkout or /cartclear first.",
                )
                .await?;
                return Ok(());
            }
            match db.cart_add_item(user_id, seller_id, item_type, item_id, qty) {
                Ok(()) => {
                    db.mark_cart_abandoned(user_id, seller_id);
                    bot.send_message(chat_id, format!("Added {} x{} to cart.", name, qty))
                        .await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Error: {}", e)).await?;
                }
            }
        }
        Command::CartRemove(arg) => {
            let name = arg.trim();
            if name.is_empty() {
                bot.send_message(chat_id, "Usage: /cartremove <name>")
                    .await?;
                return Ok(());
            }
            let (plans, products) = resolve_buy_candidates(&db, role, user_id, name);
            if plans.len() + products.len() != 1 {
                bot.send_message(chat_id, "Unknown item.").await?;
                return Ok(());
            }
            let (item_type, item_id, seller_id) = if let Some(product) = products.into_iter().next()
            {
                let (pid, pseller, ..) = product;
                ("product", pid, pseller)
            } else if let Some(plan) = plans.into_iter().next() {
                let (pname, .., pseller) = plan;
                let (pid, ..) = match db.get_plan(pseller, &pname) {
                    Some(x) => x,
                    None => {
                        bot.send_message(chat_id, "Plan not found.").await?;
                        return Ok(());
                    }
                };
                ("plan", pid, pseller)
            } else {
                bot.send_message(chat_id, "Unknown item.").await?;
                return Ok(());
            };
            if db.cart_remove_item(user_id, seller_id, item_type, item_id) {
                bot.send_message(chat_id, "Removed from cart.").await?;
            } else {
                bot.send_message(chat_id, "Item not in cart.").await?;
            }
        }
        Command::CartClear => {
            db.cart_clear_all(user_id);
            bot.send_message(chat_id, "Cart cleared.").await?;
        }
        Command::Checkout => {
            let items = db.cart_get_all_items(user_id);
            if items.is_empty() {
                bot.send_message(chat_id, "Your cart is empty.").await?;
                return Ok(());
            }
            // Resolve all items.
            let mut resolved2: Vec<(i64, &'static str, i64, String, String, String, i64)> =
                Vec::new();
            let mut assets = std::collections::BTreeSet::new();
            let mut sellers = std::collections::BTreeSet::new();
            let mut ok = true;
            for (seller_id, _row_id, item_type, item_id, qty) in &items {
                sellers.insert(*seller_id);
                let (name, price, asset) = if item_type == "plan" {
                    match db.get_plan_by_id(*item_id) {
                        Some((_, p, a, n, _, _)) => (n, p, a),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    let prod = db
                        .list_products(Some(*seller_id))
                        .into_iter()
                        .find(|p| p.0 == *item_id);
                    match prod {
                        Some(p) => (p.2, p.3, p.4),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                };
                assets.insert(asset.clone());
                resolved2.push((
                    *seller_id,
                    if item_type == "plan" {
                        "plan"
                    } else {
                        "product"
                    },
                    *item_id,
                    name,
                    price,
                    asset,
                    *qty,
                ));
            }
            if !ok {
                bot.send_message(chat_id, "Cart contains an unknown item. Use /cartclear.")
                    .await?;
                return Ok(());
            }
            if sellers.len() != 1 {
                bot.send_message(
                    chat_id,
                    "Cart contains items from multiple sellers. Checkout separately.",
                )
                .await?;
                return Ok(());
            }
            if assets.len() != 1 {
                bot.send_message(
                    chat_id,
                    "Cart contains mixed currencies. Checkout separately.",
                )
                .await?;
                return Ok(());
            }
            let seller_id = *sellers.iter().next().unwrap();
            let asset = assets.iter().next().unwrap().clone();
            // total
            let mut total: f64 = 0.0;
            for r in &resolved2 {
                let p: f64 = r.4.parse().unwrap_or(0.0);
                total += p * r.6 as f64;
            }
            let amount = format!("{:.2}", total);
            // crypto
            let cp = crypto_or_load(&db, &cfg, crypto.clone());
            match cp {
                Some(c) => {
                    let desc = "Cart checkout".to_string();
                    match c
                        .create_invoice(&amount, &asset, Some(&desc), Some("viewItem"), None)
                        .await
                    {
                        Ok(v) => {
                            let inv_id = v
                                .get("invoice_id")
                                .map(|x| x.to_string())
                                .unwrap_or_default();
                            let pay_url = v.get("pay_url").and_then(|x| x.as_str()).unwrap_or("");
                            if !inv_id.is_empty() {
                                db.save_invoice(
                                    &inv_id,
                                    seller_id,
                                    &amount,
                                    &asset,
                                    Some(&desc),
                                    "active",
                                    Some(pay_url),
                                    Some("cart"),
                                    Some(user_id),
                                    None,
                                );
                            }
                            send_invoice_created(
                                &bot, chat_id, "cart", &amount, &asset, pay_url, &inv_id,
                            )
                            .await?;
                        }
                        Err(e) => {
                            tracing::warn!("cart checkout invoice failed: {}", e);
                            bot.send_message(chat_id, "Something went wrong creating your invoice. Please try again in a minute.").await?;
                        }
                    }
                }
                None => {
                    tracing::warn!("cart checkout: no crypto token");
                    bot.send_message(chat_id, "This shop can't take payments right now — the seller is finishing setup. Please contact the seller.").await?;
                }
            }
        }
        Command::Paid(inv) => {
            let inv = inv.trim();
            let info = db.get_invoice(inv);
            match info {
                Some((_amount, _asset, status, _desc, plan_name, uid, inv_seller)) => {
                    if status == "paid" {
                        bot.send_message(chat_id, "Already processed.").await?;
                        return Ok(());
                    }
                    // Invoice user binding: an invoice bound to a user may only
                    // be completed by that user; an unbound invoice only by
                    // Admin/Owner (never a random caller granting themselves).
                    let binding_ok = match uid {
                        Some(u) => u == user_id || is_admin,
                        None => is_admin,
                    };
                    if !binding_ok {
                        bot.send_message(chat_id, "You may not complete this invoice.")
                            .await?;
                        return Ok(());
                    }
                    if status == "failed" {
                        // Terminal state from the background granter's bounded
                        // verification retries (P1#8): distinct, actionable
                        // copy instead of the misleading "already being
                        // processed".
                        bot.send_message(
                            chat_id,
                            format!(
                                "Payment verification failed permanently (the crypto check didn't confirm in time). Contact the seller with invoice {} — do not pay again before they confirm.",
                                inv
                            ),
                        )
                        .await?;
                        return Ok(());
                    }
                    // Claim (active -> granting) so the background granter and
                    // the claim API cannot double-grant while we verify.
                    if !db.claim_invoice_for_grant(inv) {
                        bot.send_message(
                            chat_id,
                            "Invoice is already being processed. Try again shortly.",
                        )
                        .await?;
                        return Ok(());
                    }
                    // Fail-closed: no crypto token means no payment
                    // verification, so no grant. Release the claim for a later
                    // attempt.
                    let cp = crypto_or_load(&db, &cfg, crypto.clone());
                    let c = match cp {
                        Some(c) => c,
                        None => {
                            db.release_invoice_claim(inv);
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Payment verification isn't available for this shop yet — the seller is finishing setup. Contact the seller with invoice {}.",
                                    inv
                                ),
                            )
                            .await?;
                            return Ok(());
                        }
                    };
                    match c.get_invoice(inv).await {
                        Err(e) => {
                            // Verification error: release the claim; the
                            // background granter retries. Never grant on doubt.
                            db.release_invoice_claim(inv);
                            tracing::warn!(invoice = inv, error = %e, "paid check failed");
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Payment couldn't be verified just now. Try /paid {} again in a minute, or contact the seller.",
                                    inv
                                ),
                            )
                            .await?;
                        }
                        Ok(v) => {
                            let paid = crate::api::extract_invoice_items(&v)
                                .iter()
                                .any(|x| x.get("status").and_then(|s| s.as_str()) == Some("paid"));
                            if !paid {
                                db.release_invoice_claim(inv);
                                bot.send_message(chat_id, "Invoice not paid yet.").await?;
                                return Ok(());
                            }
                            // Verified paid. Product invoices take the
                            // delivery path; plan invoices the subscription
                            // path. The branch is selected by
                            // invoices.product_id.
                            if let Some((pid, kind, pname, delivery)) =
                                db.invoice_product_delivery(inv)
                            {
                                let target_user = uid.unwrap_or(user_id);
                                if db.product_grant_exists(inv) {
                                    db.finalize_invoice_paid(inv);
                                    bot.send_message(
                                        chat_id,
                                        "Payment confirmed (product already delivered).",
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                match db.grant_product(inv_seller, target_user, pid, inv) {
                                    Ok(true) => {
                                        // Physical goods also open a
                                        // fulfillment order the seller
                                        // manages with /orders. Best-effort:
                                        // the grant is already durable, so a
                                        // failure (e.g. duplicate invoice) is
                                        // only logged.
                                        if kind == "physical"
                                            && let Err(e) =
                                                db.create_order(inv_seller, pid, target_user, inv)
                                        {
                                            tracing::warn!(
                                                "invoice {}: order creation failed ({})",
                                                inv,
                                                e
                                            );
                                        }
                                        match crate::services::deliver::deliver(
                                            &bot,
                                            target_user,
                                            &kind,
                                            &pname,
                                            delivery.as_deref(),
                                            &db,
                                            inv_seller,
                                            Some(bot_ctx.id),
                                        )
                                        .await
                                        {
                                            Ok(()) => {
                                                bot.send_message(
                                                    chat_id,
                                                    format!(
                                                        "Payment confirmed. Product '{}' delivered to your DM.",
                                                        pname
                                                    ),
                                                )
                                                .await?;
                                            }
                                            Err(e) => {
                                                bot.send_message(
                                                    chat_id,
                                                    format!(
                                                        "Payment confirmed, but delivery failed: {}. Contact the seller.",
                                                        e
                                                    ),
                                                )
                                                .await?;
                                            }
                                        }
                                    }
                                    // A concurrent granter won the
                                    // UNIQUE(invoice_id) race; nothing to do.
                                    Ok(false) => {}
                                    // e.g. out of stock: the money is settled
                                    // but the grant is not — surface it and
                                    // finalize so the invoice stops
                                    // re-queueing.
                                    Err(e) => {
                                        tracing::warn!(
                                            "invoice {}: product grant failed ({})",
                                            inv,
                                            e
                                        );
                                        bot.send_message(
                                            chat_id,
                                            format!(
                                                "Payment confirmed, but grant failed: {}. Contact the seller.",
                                                e
                                            ),
                                        )
                                        .await?;
                                    }
                                }
                                db.finalize_invoice_paid(inv);
                                return Ok(());
                            }
                            // Cart invoices: grant every item in the buyer's cart.
                            if plan_name.as_deref() == Some("cart") {
                                let target_user = uid.unwrap_or(user_id);
                                let cart_items = db.cart_get_all_items(target_user);
                                if cart_items.is_empty() {
                                    db.finalize_invoice_paid(inv);
                                    bot.send_message(
                                        chat_id,
                                        "Payment confirmed (cart already cleared).",
                                    )
                                    .await?;
                                    return Ok(());
                                }
                                // Idempotency: if already granted, finalize and return.
                                // (v1 minimal: no per-invoice grant tracking for cart;
                                //  we just grant and clear.)
                                let mut granted_msgs: Vec<String> = Vec::new();
                                for (seller_id, _row_id, item_type, item_id, qty) in &cart_items {
                                    if item_type == "plan" {
                                        if let Some((sid, _price, _asset, pname, days, ch)) =
                                            db.get_plan_by_id(*item_id)
                                        {
                                            let ch = ch.unwrap_or_else(|| {
                                                db.seller_channel(sid).unwrap_or_default()
                                            });
                                            let days = days.clamp(1, MAX_PLAN_DAYS);
                                            if !ch.is_empty() {
                                                let sub_id = db.create_subscription(
                                                    sid,
                                                    target_user,
                                                    Some(*item_id),
                                                    &ch,
                                                    days,
                                                    Some(inv),
                                                    None,
                                                );
                                                let link =
                                                    grant_access(&bot, &ch, target_user, days)
                                                        .await;
                                                match link {
                                                    Ok(l) => granted_msgs.push(format!(
                                                        "Plan '{}' x{}: access granted (sub #{}). Invite: {}",
                                                        pname, qty, sub_id, l
                                                    )),
                                                    Err(e) => {
                                                        tracing::warn!("cart plan grant invite failed: {}", e);
                                                        granted_msgs.push(format!(
                                                            "Plan '{}' x{}: access granted (sub #{}), invite link failed.",
                                                            pname, qty, sub_id
                                                        ));
                                                    }
                                                }
                                            } else {
                                                tracing::warn!(
                                                    "cart plan: seller {} has no channel",
                                                    sid
                                                );
                                            }
                                        }
                                        let _ = seller_id;
                                    } else {
                                        // product
                                        let prod = db
                                            .list_products(Some(*seller_id))
                                            .into_iter()
                                            .find(|p| p.0 == *item_id);
                                        if let Some(p) = prod {
                                            let (
                                                _pid,
                                                pseller,
                                                pname,
                                                _price,
                                                _asset,
                                                kind,
                                                delivery,
                                                _stock,
                                                _active,
                                            ) = p;
                                            // grant qty times
                                            for _ in 0..*qty {
                                                let _ = db.grant_product(
                                                    pseller,
                                                    target_user,
                                                    *item_id,
                                                    inv,
                                                );
                                            }
                                            if kind == "physical" {
                                                let _ = db.create_order(
                                                    pseller,
                                                    *item_id,
                                                    target_user,
                                                    inv,
                                                );
                                            }
                                            match crate::services::deliver::deliver(
                                                &bot,
                                                target_user,
                                                &kind,
                                                &pname,
                                                delivery.as_deref(),
                                                &db,
                                                pseller,
                                                Some(bot_ctx.id),
                                            )
                                            .await
                                            {
                                                Ok(()) => granted_msgs.push(format!(
                                                    "Product '{}' x{} delivered.",
                                                    pname, qty
                                                )),
                                                Err(e) => {
                                                    granted_msgs.push(format!(
                                                        "Product '{}' x{}: delivery failed: {}.",
                                                        pname, qty, e
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                                if let Some(first) = cart_items.first() {
                                    db.mark_cart_recovered(target_user, first.0);
                                }
                                db.cart_clear_all(target_user);
                                db.finalize_invoice_paid(inv);
                                if granted_msgs.is_empty() {
                                    bot.send_message(chat_id, "Payment confirmed.").await?;
                                } else {
                                    bot.send_message(
                                        chat_id,
                                        format!("Payment confirmed.\n{}", granted_msgs.join("\n")),
                                    )
                                    .await?;
                                }
                                return Ok(());
                            }
                            // Verified paid. Grant once (idempotent), then
                            // finalize.
                            if db.subscription_exists_for_invoice(inv) {
                                db.finalize_invoice_paid(inv);
                                bot.send_message(
                                    chat_id,
                                    "Payment confirmed (subscription already granted).",
                                )
                                .await?;
                                return Ok(());
                            }
                            let target_user = uid.unwrap_or(user_id);
                            // The channel must belong to the invoice's seller
                            // (via its plan) — never a global default.
                            let (days, ch) = if let Some(pn) = &plan_name
                                && let Some((_pid, _pr, _as, _nm, d, c)) =
                                    db.get_plan(inv_seller, pn)
                            {
                                (
                                    d,
                                    c.unwrap_or_else(|| {
                                        db.seller_channel(inv_seller).unwrap_or_default()
                                    }),
                                )
                            } else {
                                (30i64, db.seller_channel(inv_seller).unwrap_or_default())
                            };
                            let days = days.clamp(1, MAX_PLAN_DAYS);
                            if ch.is_empty() {
                                // Paid but ungrantable here: release the claim
                                // ('granting' -> 'active') so the background
                                // granter keeps retrying once the seller
                                // configures a channel — the invoice must never
                                // wedge in 'granting' (P0#3).
                                db.release_invoice_claim(inv);
                                tracing::info!(
                                    "invoice {}: paid but seller {} has no channel; claim released for retry",
                                    inv,
                                    inv_seller
                                );
                                bot.send_message(
                                    chat_id,
                                    "The seller's channel isn't configured yet. Your payment will be delivered automatically once they finish setup — no action needed from you.",
                                )
                                .await?;
                                return Ok(());
                            }
                            let sub_id = db.create_subscription(
                                inv_seller,
                                target_user,
                                None,
                                &ch,
                                days,
                                Some(inv),
                                None,
                            );
                            let link = grant_access(&bot, &ch, target_user, days).await;
                            match link {
                                Ok(l) => bot.send_message(chat_id, format!("Payment confirmed. Access granted.\nInvite: {}\nSubscription #{}", l, sub_id)).await?,
                                Err(e) => {
                                    // Buyer-safe copy (P2#14): the raw Telegram
                                    // error never reaches the buyer; it goes to
                                    // the log only.
                                    tracing::warn!(
                                        "invoice {}: subscription granted but invite link failed: {}",
                                        inv,
                                        e
                                    );
                                    bot.send_message(
                                        chat_id,
                                        "Access granted, but the invite link couldn't be created — ask the seller to re-invite you.",
                                    )
                                    .await?
                                }
                            };
                            db.finalize_invoice_paid(inv);
                        }
                    }
                }
                None => {
                    bot.send_message(chat_id, "Unknown invoice.").await?;
                }
            }
        }
        Command::Redeem(code) => {
            let code = code.trim();
            // Pre-flight (P1#7): redeem_code burns a use unconditionally, so
            // the code's seller must have a channel to grant INTO before we
            // consume it. An unknown code falls through to redeem_code for
            // its normal "invalid code" error.
            if let Some(c) = db.get_code(code)
                && !db.seller_has_channel(c.1)
            {
                bot.send_message(
                    chat_id,
                    "This code is valid, but the seller hasn't finished channel setup. Ask the seller to run /setchannel, then redeem again.",
                )
                .await?;
                return Ok(());
            }
            match db.redeem_code(code, user_id) {
                Ok((code_seller, plan_id, value_days)) => {
                    // Days and channel come from the code's own plan/seller —
                    // never from a global default the seller does not control
                    // (plain users redeeming must still reach the seller's
                    // channel).
                    let (seller_id, d, ch) = if let Some(pid) = plan_id {
                        if let Some((sid, _price, _asset, _name, days, ch)) = db.get_plan_by_id(pid)
                        {
                            (
                                sid,
                                days,
                                ch.unwrap_or_else(|| db.seller_channel(sid).unwrap_or_default()),
                            )
                        } else {
                            (
                                code_seller,
                                value_days.unwrap_or(30),
                                db.seller_channel(code_seller).unwrap_or_default(),
                            )
                        }
                    } else {
                        (
                            code_seller,
                            value_days.unwrap_or(30),
                            db.seller_channel(code_seller).unwrap_or_default(),
                        )
                    };
                    let d = d.clamp(1, MAX_PLAN_DAYS);
                    if ch.is_empty() {
                        bot.send_message(
                            chat_id,
                            "Code redeemed, but the seller has no channel configured. Contact the seller.",
                        )
                        .await?;
                        return Ok(());
                    }
                    let sub_id =
                        db.create_subscription(seller_id, user_id, plan_id, &ch, d, None, None);
                    let link = grant_access(&bot, &ch, user_id, d).await;
                    match link {
                        Ok(l) => {
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Code redeemed. Access granted.\nInvite: {}\nSubscription #{}",
                                    l, sub_id
                                ),
                            )
                            .await?
                        }
                        Err(e) => {
                            // Buyer-safe copy (P2#14): the raw Telegram error
                            // never reaches the buyer; it goes to the log only.
                            tracing::warn!(
                                "code redeemed for user {} but invite link failed: {}",
                                user_id,
                                e
                            );
                            bot.send_message(
                                chat_id,
                                "Access granted, but the invite link couldn't be created — ask the seller to re-invite you.",
                            )
                            .await?
                        }
                    };
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("Redeem failed: {}", e))
                        .await?;
                }
            }
        }
        Command::GenCodes(arg) => {
            match parse_gen_codes(&arg) {
                None => {
                    bot.send_message(chat_id, "Usage: /gencodes plan | count | [days]")
                        .await?;
                }
                Some((plan_name, count, days)) => {
                    // Seller-scoped lookup: the caller may only mint codes against
                    // their own plans.
                    let (seller_id, plan_id, val_days) = match db.get_plan(user_id, plan_name) {
                        Some((pid, _p, _a, _n, d, _c)) => (user_id, Some(pid), Some(d)),
                        None => (user_id, None, days),
                    };
                    let mut out = String::from("Generated codes:\n");
                    for _ in 0..count.min(50) {
                        let code = db.create_code(seller_id, plan_id, val_days, 1, None, 12);
                        out.push_str(&code);
                        out.push('\n');
                    }
                    bot.send_message(chat_id, out).await?;
                }
            }
        }
        Command::Codes => {
            let codes = db.list_codes(if is_admin { None } else { Some(user_id) }, 50);
            if codes.is_empty() {
                bot.send_message(chat_id, "No codes.").await?;
            } else {
                let mut out = String::from("Codes (code | plan_id | value_days | uses/max):\n");
                for (code, pid, vd, uses, max, _seller, _revoked) in codes {
                    out.push_str(&format!(
                        "{} | {:?} | {:?} | {}/{}\n",
                        code, pid, vd, uses, max
                    ));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::Grant(arg) => {
            let parts: Vec<&str> = arg.split('|').map(|s| s.trim()).collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /grant user_id | days")
                    .await?;
            } else {
                let uid: i64 = parts[0].parse().unwrap_or(0);
                let days: i64 = parts[1].parse().unwrap_or(30).clamp(1, MAX_PLAN_DAYS);
                if uid == 0 {
                    bot.send_message(chat_id, "Invalid user id.").await?;
                } else {
                    // Sellers grant only into their own channel; Admin/Owner
                    // may fall back to the global default.
                    let ch = channel_of(&db, &cfg, Some(user_id), is_admin);
                    if ch.is_empty() {
                        bot.send_message(
                            chat_id,
                            "No channel configured. Use /setup @channel first.",
                        )
                        .await?;
                        return Ok(());
                    }
                    let sub_id = db.create_subscription(user_id, uid, None, &ch, days, None, None);
                    let link = grant_access(&bot, &ch, uid, days).await;
                    match link {
                        Ok(l) => {
                            bot.send_message(
                                chat_id,
                                format!("Granted. Invite: {}\nSub #{}", l, sub_id),
                            )
                            .await?
                        }
                        Err(e) => {
                            tracing::warn!(sub_id, invite_error = %e, "invite link creation failed");
                            bot.send_message(
                                chat_id,
                                "Access granted, but the invite link couldn't be created — run /invite again for this user.",
                            )
                            .await?
                        }
                    };
                }
            }
        }
        Command::Revoke(arg) => {
            let uid: i64 = arg.trim().parse().unwrap_or(0);
            if uid == 0 {
                bot.send_message(chat_id, "Usage: /revoke user_id").await?;
            } else {
                let ch = channel_of(&db, &cfg, Some(user_id), is_admin);
                // ban then unban (kick)
                let cid = util::to_chat_id(&ch);
                let _ = bot
                    .ban_chat_member(cid.clone(), teloxide::types::UserId(uid as u64))
                    .await;
                let _ = bot
                    .unban_chat_member(cid, teloxide::types::UserId(uid as u64))
                    .await;
                bot.send_message(chat_id, format!("Revoked access for {}.", uid))
                    .await?;
            }
        }
        Command::Subs => {
            let subs = db.list_subscriptions(if is_admin { None } else { Some(user_id) }, 50);
            if subs.is_empty() {
                bot.send_message(chat_id, "No subscriptions.").await?;
            } else {
                let mut out = String::from("Subs (id | user | channel | status | expires):\n");
                for (id, _seller, uid, ch, st, exp) in subs {
                    out.push_str(&format!("{} | {} | {} | {} | {}\n", id, uid, ch, st, exp));
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::ServiceAdd(arg) => {
            let parts: Vec<&str> = arg.split('|').map(|s| s.trim()).collect();
            if parts.len() < 4 {
                bot.send_message(
                    chat_id,
                    "Usage: /serviceadd name | price | asset | duration_minutes [| channel]",
                )
                .await?;
            } else {
                let name = parts[0];
                let price = parts[1];
                let asset = parts[2];
                let dur: i64 = parts[3].parse().unwrap_or(60);
                let channel = parts.get(4).copied().filter(|s| !s.is_empty());
                match db.create_service(user_id, name, price, asset, dur, channel) {
                    Ok(id) => {
                        bot.send_message(chat_id, format!("Service #{} created: {}", id, name))
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(chat_id, format!("Error: {}", e)).await?;
                    }
                };
            }
        }
        Command::Services => {
            // Now buyer-reachable (P1#6): plain users get the public listing —
            // name, duration and price only, seller-internal fields (channel,
            // internal ids) stay hidden. Sellers/admins keep their management
            // view.
            if role == roles::Role::User {
                let svcs: Vec<_> = db
                    .list_services(None)
                    .into_iter()
                    // seller_id=0 rows are admin-managed globals, never the
                    // buyer-facing bookable catalogue.
                    .filter(|s| s.1 != 0)
                    .collect();
                if svcs.is_empty() {
                    bot.send_message(chat_id, "No services listed yet — check back soon.")
                        .await?;
                } else {
                    let mut out = String::from("Services:\n");
                    for (_id, seller, name, price, asset, mins, _ch) in svcs {
                        let price_part = if price.is_empty() || price == "-" {
                            String::new()
                        } else {
                            format!(" — {} {}", price, asset)
                        };
                        out.push_str(&format!(
                            "- {} — {} min{} — by {}\n",
                            name,
                            mins,
                            price_part,
                            seller_display(&db, seller)
                        ));
                    }
                    bot.send_message(chat_id, out).await?;
                }
            } else {
                let svcs = db.list_services(if is_admin { None } else { Some(user_id) });
                if svcs.is_empty() {
                    let empty = if is_admin {
                        "No services."
                    } else {
                        "No services. Add one with /serviceadd name | price | asset | duration_minutes."
                    };
                    bot.send_message(chat_id, empty).await?;
                } else {
                    let mut out =
                        String::from("Services (id | seller | name | price | asset | mins):\n");
                    for (id, sid, name, price, asset, mins, _ch) in svcs {
                        out.push_str(&format!(
                            "{} | {} | {} | {} | {} | {}\n",
                            id, sid, name, price, asset, mins
                        ));
                    }
                    bot.send_message(chat_id, out).await?;
                }
            }
        }
        Command::SlotAdd(arg) => {
            let parts: Vec<&str> = arg.split('|').map(|s| s.trim()).collect();
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /slotadd service | datetime [| capacity]")
                    .await?;
            } else {
                let svc_name = parts[0];
                let dt_raw = parts[1];
                // D4 (v1.4): the stored time must be unambiguous and future —
                // RFC3339 or "YYYY-MM-DD HH:MM" (read as UTC and normalized to
                // RFC3339 so create_booking's parser accepts it). Everything
                // else, and any past timestamp, is rejected up front.
                match parse_slot_datetime(dt_raw).filter(|dt| *dt > Utc::now()) {
                    Some(dt) => {
                        // D1: one slot holds exactly one booking in v1.4. The
                        // capacity argument stays for CLI stability but is
                        // clamped to 1, with the spec's notice.
                        let cap_arg = parts.get(2).and_then(|s| s.parse::<i64>().ok());
                        let clamp = cap_arg.map(|c| c > 1).unwrap_or(false);
                        let cap = if clamp { 1 } else { cap_arg.unwrap_or(1) };
                        match db.find_service_id(user_id, svc_name) {
                            Some(sid) => {
                                let rfc = dt.to_rfc3339();
                                let id = db.create_slot(sid, user_id, &rfc, cap);
                                let mut out =
                                    format!("Slot #{} created for {} at {}.", id, svc_name, dt_raw);
                                if clamp {
                                    out.push_str("\nCapacity > 1 is reserved for class bookings (v1.5); slot created with capacity 1.");
                                }
                                bot.send_message(chat_id, out).await?;
                            }
                            None => {
                                bot.send_message(chat_id, "Service not found.").await?;
                            }
                        }
                    }
                    None => {
                        bot.send_message(
                            chat_id,
                            "Could not create the slot. Use a future UTC time, format YYYY-MM-DD HH:MM.",
                        )
                        .await?;
                    }
                }
            }
        }
        Command::Book(arg) => {
            let name = arg.trim();
            if name.is_empty() {
                bot.send_message(chat_id, "Usage: /book <service_name>")
                    .await?;
            } else {
                // Buyer-reachable (P1#6): buyers book any seller's public
                // service; sellers book within their own catalogue; admins may
                // book anything (incl. the global namespace).
                let svc = if is_admin {
                    db.list_services(None).into_iter().find(|s| s.2 == name)
                } else if role == roles::Role::Seller {
                    db.list_services(Some(user_id))
                        .into_iter()
                        .find(|s| s.2 == name)
                } else {
                    db.list_services(None)
                        .into_iter()
                        .find(|s| s.2 == name && s.1 != 0)
                };
                if let Some((service_id, _seller_id, svc_name, _p, _a, _d, _ch)) = svc {
                    // D1: slot status is the only bookability state — pick the
                    // first 'available' slot as the claim candidate; the past /
                    // race / scope checks all live inside create_booking's one
                    // transaction (the old handler-side availability check is
                    // gone on purpose: it was a TOCTOU).
                    let candidate = db
                        .list_slots(service_id)
                        .into_iter()
                        .find(|s| s.4 == "available");
                    match candidate {
                        Some((slot_id, starts, ..)) => {
                            match db.create_booking(service_id, slot_id, user_id) {
                                Ok(Some(bid)) => {
                                    // Hold = seller-response window (D3): the same
                                    // number the Db claims with, never a hardcoded
                                    // lie; "requested" is the truthful word.
                                    bot.send_message(
                                    chat_id,
                                    format!(
                                        "Booking #{bid} requested — {svc_name} at {starts}.\n\nThe seller has up to {} minutes to respond. You'll get a message when they confirm or decline.\nCancel anytime: /cancelbooking #{bid}",
                                        crate::db::BOOKING_HOLD_MINUTES
                                    ),
                                )
                                .await?;
                                }
                                // Race loser, past slot, or already-claimed: one
                                // truthful line, no details leaked.
                                Ok(None) => {
                                    bot.send_message(chat_id, "Slot no longer available.")
                                        .await?;
                                }
                                Err(_) => {
                                    bot.send_message(
                                    chat_id,
                                    "That slot's time is invalid — ask the seller to recreate it.",
                                )
                                .await?;
                                }
                            }
                        }
                        None => {
                            bot.send_message(chat_id, "No available slots.").await?;
                        }
                    }
                } else {
                    bot.send_message(chat_id, "Service not found.").await?;
                }
            }
        }
        Command::Bookings => {
            if role == roles::Role::User {
                // Buyers are pointed at their own view (BOOKINGS_UX_SPEC §4.7);
                // sellers/admins keep the list below.
                bot.send_message(
                    chat_id,
                    "Bookings are a seller view. See your own bookings with /mybookings.",
                )
                .await?;
            } else {
                let bs = db.list_bookings(if is_admin { None } else { Some(user_id) });
                if bs.is_empty() {
                    bot.send_message(chat_id, "No bookings.").await?;
                } else {
                    let mut out = String::from(
                        "Bookings (id | seller | service | user | status | expires):\n",
                    );
                    for (bid, sid, svc, uid, st, exp) in bs {
                        out.push_str(&format!(
                            "{} | {} | {} | {} | {} | {}\n",
                            bid,
                            sid,
                            svc,
                            uid,
                            st,
                            exp.unwrap_or_default()
                        ));
                    }
                    bot.send_message(chat_id, out).await?;
                }
            }
        }
        Command::MyBookings => {
            let rows = db.list_bookings_for_user(user_id, 20);
            if rows.is_empty() {
                bot.send_message(
                    chat_id,
                    "You have no bookings. Browse /services and book with /book <service_name>.",
                )
                .await?;
            } else {
                let mut out = String::from("Your bookings (id | service | time | status):\n");
                let mut actionable = false;
                // The public 8-char token is the buyer-facing reference (the
                // integer id only ever appears in /book's confirmation line).
                for (_id, token, _buyer, service, starts, _ends, status, expires, seller) in rows {
                    out.push_str(&format!(
                        "{} | {} | {} | {}\n",
                        token,
                        service,
                        starts.as_deref().unwrap_or("—"),
                        status
                    ));
                    match status.as_str() {
                        "held" => {
                            actionable = true;
                            out.push_str(&format!(
                                "The seller has until {} to respond.\n",
                                expires.as_deref().unwrap_or("—")
                            ));
                        }
                        "confirmed" => {
                            actionable = true;
                            out.push_str(&format!(
                                "Seller: {}.\n",
                                seller_display(&db, seller.unwrap_or(0))
                            ));
                        }
                        _ => {}
                    }
                }
                if actionable {
                    out.push_str("Cancel: /cancelbooking <booking_id>\n");
                }
                bot.send_message(chat_id, out).await?;
            }
        }
        Command::CancelBooking(arg) => {
            let key = arg.trim();
            if key.is_empty() {
                bot.send_message(chat_id, "Usage: /cancelbooking <booking_id>")
                    .await?;
            } else {
                // Ownership check lives HERE, before any write: the scoped
                // cancel accepts an admin/owner scope (None), so the only safe
                // path is to resolve the id against the CALLER's own list —
                // every returned row already has buyer_user_id == caller.
                // Matching by integer id or public token, nothing else.
                let owned =
                    db.list_bookings_for_user(user_id, 200)
                        .into_iter()
                        .find(|(id, token, ..)| {
                            *token == key || id.to_string() == key || format!("#{id}") == key
                        });
                match owned {
                    Some((id, _token, _buyer, service, _starts, _ends, _status, _exp, seller)) => {
                        if db.cancel_booking_scoped(id, None) {
                            bot.send_message(
                                chat_id,
                                format!(
                                    "Booking #{id} cancelled. The seller has been notified and the slot is free again."
                                ),
                            )
                            .await?;
                            // Seller DM (BOOKINGS_UX_SPEC §4.4): best-effort —
                            // a failed DM is logged, never propagated. Global
                            // services (seller_id 0) have no seller to notify.
                            if let Some(sid) = seller.filter(|s| *s != 0) {
                                match bot
                                    .send_message(
                                        teloxide::types::ChatId(sid),
                                        format!(
                                            "Booking #{id} ({service}) was cancelled by the buyer. The slot is free again."
                                        ),
                                    )
                                    .await
                                {
                                    Ok(_) => crate::metrics::bot_sent(),
                                    Err(e) => tracing::warn!(
                                        "cancelbooking: failed to DM seller {} about cancelled booking #{}: {}",
                                        sid,
                                        id,
                                        e
                                    ),
                                }
                            }
                        } else {
                            // Terminal (declined/cancelled) or lost a race —
                            // one no-oracle message per the spec.
                            bot.send_message(chat_id, "Booking not found or already closed.")
                                .await?;
                        }
                    }
                    None => {
                        bot.send_message(chat_id, "Booking not found or already closed.")
                            .await?;
                    }
                }
            }
        }
        Command::BookingConfirm(arg) => {
            if is_admin {
                // Admins/owner respond via the console /bookings page, not the
                // bot: confirming another seller's booking is out of scope for
                // the bot on purpose.
                bot.send_message(
                    chat_id,
                    format!(
                        "Booking confirm/decline are seller commands. Admins and owners respond to all bookings in the web console: {}/bookings",
                        cfg.web_url()
                    ),
                )
                .await?;
            } else {
                booking_respond(&bot, &db, chat_id, user_id, &arg, true).await?;
            }
        }
        Command::BookingDecline(arg) => {
            if is_admin {
                bot.send_message(
                    chat_id,
                    format!(
                        "Booking confirm/decline are seller commands. Admins and owners respond to all bookings in the web console: {}/bookings",
                        cfg.web_url()
                    ),
                )
                .await?;
            } else {
                booking_respond(&bot, &db, chat_id, user_id, &arg, false).await?;
            }
        }
        Command::ExpireCheck => {
            // Shared with the periodic expiry sweeper in main.rs so the
            // kick+mark behavior stays single-sourced; the shared path
            // resolves the moderating bot per row (`BotManager::for_seller`),
            // so a seller bound to another bot is kicked/DM'd by that bot, not
            // by whichever bot received the command.
            let expired = crate::expire_subscriptions(&bot_manager, &db).await;
            let mut notified = 0usize;
            for (_sub_id, seller_id, uid, channel) in &expired {
                let row_bot = bot_manager.for_seller(*seller_id).bot.clone();
                match row_bot
                    .send_message(
                        teloxide::types::ChatId(*uid),
                        format!(
                            "Your access to {channel} has expired. To renew, use /plans or contact the seller."
                        ),
                    )
                    .await
                {
                    Ok(_) => {
                        crate::metrics::bot_sent();
                        notified += 1;
                    }
                    Err(e) => tracing::warn!(
                        "failed to DM user {} about expired access to {}: {}",
                        uid,
                        channel,
                        e
                    ),
                }
            }
            let mut out = format!("Expired and revoked: {}", expired.len());
            if !expired.is_empty() {
                out.push_str(&format!(
                    " ({} buyer{} notified)",
                    notified,
                    if notified == 1 { "" } else { "s" }
                ));
            }
            bot.send_message(chat_id, out).await?;
        }
        Command::Broadcast(raw) => {
            // Parse optional --segment flag: /broadcast --segment <seg> <text>
            // or /broadcast --segment=<seg> <text>.
            let mut segment = "all".to_string();
            let mut parts: Vec<&str> = raw.split_whitespace().collect();
            if parts.first() == Some(&"--segment") {
                if parts.len() >= 2 {
                    segment = parts[1].to_string();
                    parts.drain(0..2);
                }
            } else if let Some(first) = parts.first().copied()
                && let Some(seg) = first.strip_prefix("--segment=")
            {
                segment = seg.to_string();
                parts.remove(0);
            }
            let text = parts.join(" ").trim().to_string();
            // Role-scoped audience: Admin/Owner reach all registered users,
            // sellers only their own customers (subscription holders). The
            // seller branch was previously unreachable behind an admin-only
            // early return; the docs (docs/COMMANDS.md, README) promise
            // seller-scoped broadcasts.
            if text.is_empty() {
                bot.send_message(chat_id, "Usage: /broadcast [--segment <seg>] <text>")
                    .await?;
            } else {
                let seller_scope = if is_admin { None } else { Some(user_id) };
                let users =
                    crate::services::broadcast::filter_broadcast_users(&db, seller_scope, &segment);
                if users.is_empty() {
                    bot.send_message(chat_id, "No users to broadcast to (no customers found).")
                        .await?;
                } else {
                    // Outbound gate before the raw-Bot broadcast fan-out
                    // (non-ContentSender path, same shared loop as MCP).
                    if crate::handlers::moderation::gate_outbound(
                        &db,
                        user_id,
                        Some(bot_ctx.id),
                        &text,
                    )
                    .is_err()
                    {
                        return Ok(());
                    }
                    // Send loop shared with the MCP broadcast tool (crate::broadcast_to_users).
                    let sent = crate::broadcast_to_users(&bot, &users, &text).await;
                    bot.send_message(chat_id, format!("Broadcast sent to {} users.", sent))
                        .await?;
                }
            }
        }
        Command::Ai(arg) => {
            if !is_admin {
                return Ok(());
            }
            if ai.is_none() {
                bot.send_message(chat_id, "AI is disabled. Set AI_ENABLED=1 and AI_API_KEY.")
                    .await?;
                return Ok(());
            }
            let ai = ai.as_ref().unwrap();
            // parse: template | key=val | ...
            let parts: Vec<&str> = arg.split('|').map(|s| s.trim()).collect();
            if parts.is_empty() || parts[0].is_empty() {
                bot.send_message(chat_id, "Usage: /ai <template> | key=val | ...")
                    .await?;
                return Ok(());
            }
            let template = parts[0];
            let mut vars = std::collections::HashMap::new();
            for p in &parts[1..] {
                if let Some((k, v)) = p.split_once('=') {
                    vars.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            match ai
                .generate(user_id, Some(user_id), template, vars, 300)
                .await
            {
                Ok(text) => {
                    bot.send_message(chat_id, text).await?;
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("AI error: {}", e))
                        .await?;
                }
            }
        }
        Command::AiPost(arg) => {
            if ai.is_none() {
                bot.send_message(chat_id, "AI is disabled. Set AI_ENABLED=1 and AI_API_KEY.")
                    .await?;
                return Ok(());
            }
            let (chan, rest) = split_channel_arg(&arg);
            // Tenancy gate first: an Err here must not create an AI draft
            // pointing at another seller's channel (the aipost:publish
            // callback sends to the draft's channel verbatim).
            let ch = match resolve_target_channel(&db, &cfg, user_id, is_admin, chan) {
                Ok(ch) => ch,
                Err(m) => {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
            };
            if ch.is_empty() {
                bot.send_message(
                    chat_id,
                    "Usage: /aipost [@channel] <template> [| key=val ...]",
                )
                .await?;
                return Ok(());
            }
            let parts: Vec<&str> = rest.split('|').map(|s| s.trim()).collect();
            if parts.is_empty() || parts[0].is_empty() {
                bot.send_message(
                    chat_id,
                    "Usage: /aipost [@channel] <template> [| key=val ...]",
                )
                .await?;
                return Ok(());
            }
            let template = parts[0];
            let mut vars = std::collections::HashMap::new();
            for p in &parts[1..] {
                if let Some((k, v)) = p.split_once('=') {
                    vars.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            let ai = ai.as_ref().unwrap();
            match ai
                .generate(user_id, Some(user_id), template, vars, 300)
                .await
            {
                Ok(text) => {
                    let draft_id =
                        db.create_ai_post_draft_with_template(user_id, &ch, Some(template), &text);
                    dialogue
                        .update(State::AwaitingAiPostConfirm { draft_id })
                        .await?;
                    let preview = format!("Draft #{} (channel {}):\n\n{}", draft_id, ch, text);
                    let kb = InlineKeyboardMarkup::new(vec![vec![
                        InlineKeyboardButton::callback(
                            "Publish",
                            format!("aipost:publish:{}", draft_id),
                        ),
                        InlineKeyboardButton::callback("Edit", format!("aipost:edit:{}", draft_id)),
                        InlineKeyboardButton::callback(
                            "Cancel",
                            format!("aipost:cancel:{}", draft_id),
                        ),
                    ]]);
                    bot.send_message(chat_id, preview).reply_markup(kb).await?;
                    let role_str = match role {
                        roles::Role::Owner => "owner",
                        roles::Role::Admin => "admin",
                        roles::Role::Seller => "seller",
                        roles::Role::User => "user",
                    };
                    db.log_action(
                        user_id,
                        role_str,
                        "ai_post_draft_created",
                        "ai_post_draft",
                        draft_id,
                        Some(template),
                    );
                }
                Err(e) => {
                    bot.send_message(chat_id, format!("AI error: {}", e))
                        .await?;
                }
            }
        }
        Command::Setup(arg) => {
            let (chan, _) = split_channel_arg(&arg);
            let ch = chan
                .or_else(|| Some(arg.trim().to_string()))
                .filter(|s| !s.is_empty());
            if let Some(c) = ch {
                // Proof of control (L10 BLOCK-3) on the seller onboarding
                // path: /setup registers a seller AND binds their channel, so
                // it gets the same non-admin rules as /setchannel. The sender
                // is present in this chat by definition, but the CHANNEL must
                // still be one they administer (and one the bot is a member
                // of). Admins/owner keep the unvalidated bootstrap path.
                if !is_admin && let Some(m) = check_seller_channel_target(&bot, &c, user_id).await {
                    bot.send_message(chat_id, m).await?;
                    return Ok(());
                }
                db.set_seller_channel(user_id, &c);
                db.upsert_seller(
                    user_id,
                    msg.from
                        .as_ref()
                        .and_then(|u| u.username.clone())
                        .as_deref(),
                    Some(&c),
                );
                bot.send_message(chat_id, format!("Setup complete. Default channel set to {}. Next: /planadd name | price | asset | days", c)).await?;
            } else {
                bot.send_message(chat_id, "Usage: /setup @yourchannel")
                    .await?;
            }
        }
        Command::BecomeSeller => {
            let uname = msg.from.as_ref().and_then(|u| u.username.clone());
            db.upsert_seller(user_id, uname.as_deref(), None);
            // provision web dashboard account if missing
            let username = uname
                .clone()
                .unwrap_or_else(|| format!("seller_{}", user_id));
            if db.get_web_account_by_username(&username).is_none() {
                // The placeholder password is written to the account row but
                // never displayed when setup links are available (D7): the DM
                // carries only the one-time claim link.
                let password = util::gen_token(12);
                if let Err(e) = db
                    .create_web_account(&username, &password, "seller", Some(user_id))
                    .await
                {
                    bot.send_message(
                        chat_id,
                        format!("Seller created, but web account failed: {}", e),
                    )
                    .await?;
                } else {
                    let link = db
                        .get_web_account_by_username(&username)
                        .and_then(|(id, ..)| setup_claim_url(&db, id));
                    if let Some(url) = link {
                        bot.send_message(chat_id, format!(
                            "You are now a seller. Use /setup @channel to link your channel.\n\nSet up your web dashboard with this one-time link (valid {} minutes):\n{url}\n\nDashboard username: {username}\nThe link lets you choose a password and signs you in. It works once — if it expires, send /dashboard for a new one. Never share it.",
                            crate::db::SETUP_LINK_TTL_MINUTES
                        )).await?;
                        return Ok(());
                    }
                    // WEB_URL unset: the legacy plaintext fallback, with the
                    // one-line operator hint (and a warn — the token-less link
                    // flow is the intended posture). The raw setup token is
                    // never logged; here none was minted.
                    tracing::warn!(
                        "WEB_URL is not set: /becomeseller DM-ed a plaintext dashboard password to account '{username}'; set WEB_URL to receive secure setup links instead of passwords."
                    );
                    let url = cfg.web_url();
                    bot.send_message(chat_id, format!(
                        "You are now a seller. Use /setup @channel to link your channel.\n\nWeb dashboard login:\nURL: {url}\nUsername: {username}\nPassword: {password}\nChange with /setwebpassword.\n\nSet WEB_URL to receive secure setup links instead of passwords."
                    )).await?;
                    return Ok(());
                }
            }
            bot.send_message(chat_id, "You are now a seller. Use /setup @channel to link your channel. Dashboard account already exists; send /dashboard for a sign-in link.").await?;
        }
        Command::SetWebPassword(newpw) => {
            let uname = msg
                .from
                .as_ref()
                .and_then(|u| u.username.clone())
                .unwrap_or_else(|| format!("seller_{}", user_id));
            let newpw = newpw.trim().to_string();
            if newpw.len() < 6 {
                bot.send_message(chat_id, "Password must be at least 6 characters.")
                    .await?;
            } else if db.update_web_password(&uname, &newpw).await.is_ok() {
                bot.send_message(chat_id, "Web dashboard password updated.")
                    .await?;
            } else {
                bot.send_message(chat_id, "No web account found. Run /becomeseller first.")
                    .await?;
            }
        }
        Command::McpToken(arg) => {
            // Seller self-service for per-seller MCP bearer tokens (D6):
            // admins/owners manage only their OWN tokens here (scope is the
            // caller's seller id; there is no admin-mints-for-seller flow).
            let parts: Vec<&str> = arg.split_whitespace().collect();
            match parts.first().copied() {
                Some("create") => {
                    let name = parts
                        .get(1..)
                        .map(|rest| rest.join(" "))
                        .unwrap_or_default();
                    let name = name.trim();
                    if name.is_empty() {
                        bot.send_message(chat_id, "Usage: /mcptoken create <name>")
                            .await?;
                    } else if name.chars().count() > 64 {
                        bot.send_message(chat_id, "Token name must be 64 characters or fewer.")
                            .await?;
                    } else if db
                        .list_mcp_tokens(user_id)
                        .iter()
                        .filter(|t| t.4 == 0)
                        .count()
                        >= MCP_TOKEN_CAP
                    {
                        bot.send_message(
                            chat_id,
                            format!(
                                "Token limit reached: {MCP_TOKEN_CAP} active tokens. Revoke one with /mcptoken revoke <id>, then create again."
                            ),
                        )
                        .await?;
                    } else {
                        let (id, raw) = db.create_mcp_token(user_id, Some(name));
                        // The raw token exists in this reply and nowhere else
                        // (the Db stores the SHA-256 hash + 8-char prefix) —
                        // it is never logged or echoed again.
                        let prefix: String = raw.chars().take(8).collect();
                        bot.send_message(chat_id, format!(
                            "MCP token created — {name} (prefix {prefix}…)\n\n{raw}\n\nShown once — store it now. It authenticates as you against the MCP endpoint; see docs/API.md → MCP section.\nExpires: never"
                        )).await?;
                        tracing::info!("minted MCP token #{} for seller {}", id, user_id);
                    }
                }
                Some("list") => {
                    let rows = db.list_mcp_tokens(user_id);
                    if rows.is_empty() {
                        bot.send_message(
                            chat_id,
                            "No MCP tokens. Create one with /mcptoken create <name>.",
                        )
                        .await?;
                    } else {
                        let mut out = String::from(
                            "Your MCP tokens (id | prefix | name | created | status):\n",
                        );
                        for (id, name, prefix, created, revoked) in rows {
                            out.push_str(&format!(
                                "{} | {} | {} | {} | {}\n",
                                id,
                                prefix
                                    .map(|p| format!("{p}…"))
                                    .unwrap_or_else(|| "—".to_string()),
                                name.unwrap_or_else(|| "—".to_string()),
                                rfc3339_date(&created),
                                if revoked != 0 { "revoked" } else { "active" }
                            ));
                        }
                        bot.send_message(chat_id, out).await?;
                    }
                }
                Some("revoke") => {
                    let Some(id) = parts.get(1).and_then(|s| s.parse::<i64>().ok()) else {
                        bot.send_message(chat_id, "Usage: /mcptoken revoke <id>")
                            .await?;
                        return Ok(());
                    };
                    // Seller-scoped: an unknown id and another seller's id are
                    // the same no-oracle message; revoking an already-revoked
                    // own token is idempotent success.
                    if db.revoke_mcp_token(id, Some(user_id)) {
                        bot.send_message(
                            chat_id,
                            format!("Token {id} revoked. Clients using it get errors immediately."),
                        )
                        .await?;
                    } else {
                        bot.send_message(
                            chat_id,
                            "Token not found (or it does not belong to you).",
                        )
                        .await?;
                    }
                }
                _ => {
                    bot.send_message(
                        chat_id,
                        "MCP tokens let external tools call this shop's MCP endpoint as you — they see only your own data.\n\n/mcptoken create <name> — mint a token\n/mcptoken list — list your tokens\n/mcptoken revoke <id> — revoke a token\n\nHow to use a token: docs/API.md → MCP section.",
                    )
                    .await?;
                }
            }
        }
        Command::Dashboard => {
            let uname = msg
                .from
                .as_ref()
                .and_then(|u| u.username.clone())
                .unwrap_or_else(|| format!("seller_{}", user_id));
            match db.get_web_account_by_username(&uname) {
                Some((account_id, ..)) => {
                    // D7: a sign-in/reset link instead of login info; the DM
                    // never carries a credential.
                    if let Some(url) = setup_claim_url(&db, account_id) {
                        bot.send_message(chat_id, format!(
                            "One-time dashboard link (valid {} minutes):\n{url}\n\nDashboard username: {uname}\nOpening it lets you set a new password and signs you in. It works once — if it expires, send /dashboard again.",
                            crate::db::SETUP_LINK_TTL_MINUTES
                        )).await?;
                    } else {
                        tracing::warn!(
                            "WEB_URL is not set: /dashboard could not mint a setup link for account '{uname}'; set WEB_URL to receive secure setup links instead of passwords."
                        );
                        let url = cfg.web_url();
                        bot.send_message(
                            chat_id,
                            format!(
                                "Web dashboard: {url}\nUsername: {uname}\n(Use /setwebpassword to change password.)\n\nSet WEB_URL to receive secure setup links instead of passwords."
                            ),
                        )
                        .await?;
                    }
                }
                None => {
                    let password = util::gen_token(12);
                    match db
                        .create_web_account(&uname, &password, "seller", Some(user_id))
                        .await
                    {
                        Ok(_) => {
                            let link = db
                                .get_web_account_by_username(&uname)
                                .and_then(|(id, ..)| setup_claim_url(&db, id));
                            if let Some(url) = link {
                                bot.send_message(chat_id, format!(
                                    "Web dashboard created.\nDashboard username: {uname}\nOne-time setup link (valid {} minutes):\n{url}\n\nThe link lets you choose a password and signs you in. It works once — if it expires, send /dashboard for a new one. Never share it.",
                                    crate::db::SETUP_LINK_TTL_MINUTES
                                )).await?;
                            } else {
                                tracing::warn!(
                                    "WEB_URL is not set: /dashboard DM-ed a plaintext dashboard password to account '{uname}'; set WEB_URL to receive secure setup links instead of passwords."
                                );
                                let url = cfg.web_url();
                                bot.send_message(chat_id, format!(
                                    "Web dashboard created.\nURL: {url}\nUsername: {uname}\nPassword: {password}\n\nSet WEB_URL to receive secure setup links instead of passwords."
                                )).await?;
                            }
                        }
                        Err(e) => {
                            bot.send_message(
                                chat_id,
                                format!("Failed to create web account: {}", e),
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        Command::AddAdmin(arg) => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let parts: Vec<&str> = arg.split_whitespace().collect();
            if parts.is_empty() {
                bot.send_message(chat_id, "Usage: /addadmin <user_id> [username]")
                    .await?;
            } else {
                let uid: i64 = parts[0].parse().unwrap_or(0);
                if uid == 0 {
                    bot.send_message(chat_id, "Invalid user_id.").await?;
                } else {
                    let uname = parts.get(1).map(|s| s.to_string());
                    db.add_admin(uid, uname.as_deref());
                    bot.send_message(chat_id, format!("Admin added: {}", uid))
                        .await?;
                }
            }
        }
        Command::RemoveAdmin(arg) => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let uid: i64 = arg.trim().parse().unwrap_or(0);
            if uid == 0 {
                bot.send_message(chat_id, "Usage: /removeadmin <user_id>")
                    .await?;
            } else {
                db.remove_admin(uid);
                bot.send_message(chat_id, format!("Admin removed: {}", uid))
                    .await?;
            }
        }
        Command::Admins => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let admins = db.list_admins();
            let mut msg_text = String::from("Admins:\n");
            for a in admins {
                msg_text.push_str(&format!("- {}\n", a));
            }
            bot.send_message(chat_id, msg_text).await?;
        }
        Command::Sellers => {
            if role < roles::Role::Admin {
                return Ok(());
            }
            let sellers = db.list_sellers();
            let mut msg_text = String::from("Sellers:\n");
            for s in sellers {
                msg_text.push_str(&format!("- {}\n", s));
            }
            bot.send_message(chat_id, msg_text).await?;
        }
        Command::WebUsers => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let accounts = db.list_web_accounts();
            let mut msg_text = String::from("Web accounts:\n");
            for (u, r) in accounts {
                msg_text.push_str(&format!("- {} ({})\n", u, r));
            }
            bot.send_message(chat_id, msg_text).await?;
        }
        Command::ExportData(arg) => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let parts: Vec<&str> = arg.split_whitespace().collect();
            let user_id_target: i64 = match parts.first().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => {
                    bot.send_message(chat_id, "Usage: /exportdata <user_id> [seller_id]")
                        .await?;
                    return Ok(());
                }
            };
            let seller_id: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            let data = db.export_buyer_data(seller_id, user_id_target);
            // Telegram message limit: send truncated if needed.
            let msg = if data.chars().count() > 3500 {
                let head: String = data.chars().take(3500).collect();
                format!("{}...\n(truncated; use web /privacy for full export)", head)
            } else {
                data
            };
            bot.send_message(chat_id, msg).await?;
        }
        Command::DeleteData(arg) => {
            if role != roles::Role::Owner {
                return Ok(());
            }
            let parts: Vec<&str> = arg.split_whitespace().collect();
            let user_id_target: i64 = match parts.first().and_then(|s| s.parse().ok()) {
                Some(v) => v,
                None => {
                    bot.send_message(
                        chat_id,
                        "Usage: /deletedata <user_id> [confirm] [seller_id]",
                    )
                    .await?;
                    return Ok(());
                }
            };
            let confirm = parts.get(1).map(|s| *s == "confirm").unwrap_or(false);
            let seller_id: i64 = if confirm {
                parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0)
            } else {
                0
            };
            if !confirm {
                bot.send_message(
                    chat_id,
                    format!(
                        "This will delete ALL data for buyer {} (seller {}). To confirm, run: /deletedata {} confirm",
                        user_id_target, seller_id, user_id_target
                    ),
                )
                .await?;
                return Ok(());
            }
            let n = db.delete_buyer_data(seller_id, user_id_target);
            bot.send_message(
                chat_id,
                format!("Deleted {} records for buyer {}.", n, user_id_target),
            )
            .await?;
        }
        Command::RefundApprove(arg) => {
            if !is_admin {
                return Ok(());
            }
            handle_refund_approve(&bot, chat_id, &db, user_id, role, arg.trim()).await?;
        }
        Command::PayoutApprove(arg) => {
            if !is_admin {
                return Ok(());
            }
            handle_payout_approve(&bot, chat_id, &db, user_id, role, arg.trim()).await?;
        }
    }
    Ok(())
}

/// Resolve `/buy <name>` candidates under the (seller_id, name) plan
/// namespace (2026-10-25 review HIGH: the old lookup was global by bare name,
/// leaking pricing across sellers and mis-resolving `/buy`). B5 fallback
/// semantics, mirroring `channel_of`:
///
/// 1. The caller's OWN namespace (seller_id = user_id) always wins — a seller
///    is never shadowed by, and can never resolve into, another seller's
///    same-name plan.
/// 2. The global namespace (seller_id = 0: legacy pre-multi-tenant rows and
///    admin-created catalogue entries) is resolvable only by Admin/Owner.
/// 3. Plain buyers (Role::User) fall back to the cross-seller active shop
///    view MINUS the global namespace — public-mode /buy keeps working
///    (README "Two modes"); ambiguous names still error exactly as the v1.2
///    record's deferred seller-disambiguation syntax intends.
/// 4. The Owner may additionally resolve any seller's entry by name
///    (superuser; the review's "owner can").
///
/// Returns (plans, products) in `list_plans`/`list_products` row shapes,
/// active rows only.
fn resolve_seller_id(db: &Db, hint: &str) -> Option<i64> {
    let hint = hint.trim().trim_start_matches('@');
    if hint.is_empty() {
        return None;
    }
    if let Ok(id) = hint.parse::<i64>() {
        return Some(id);
    }
    db.get_seller_by_username(hint)
}

fn resolve_buy_candidates(
    db: &Db,
    role: roles::Role,
    user_id: i64,
    name: &str,
) -> (Vec<crate::db::PlanRow>, Vec<crate::db::ProductRow>) {
    let name = name.trim();
    // /buy seller/name disambiguation syntax: resolve directly inside the
    // named seller's namespace, bypassing the (seller_id, name) fallback
    // order used for bare names.
    if let Some((seller_part, item_part)) = name.split_once('/') {
        let item_part = item_part.trim();
        if item_part.is_empty() {
            return (Vec::new(), Vec::new());
        }
        if let Some(sid) = resolve_seller_id(db, seller_part) {
            let plans = db.find_plans_by_name(Some(sid), item_part);
            let products: Vec<crate::db::ProductRow> = db
                .find_products_by_name(Some(sid), item_part)
                .into_iter()
                .filter(|p| p.8 != 0)
                .collect();
            return (plans, products);
        }
        return (Vec::new(), Vec::new());
    }
    let mut plans = db.find_plans_by_name(Some(user_id), name);
    let mut products: Vec<crate::db::ProductRow> = db
        .find_products_by_name(Some(user_id), name)
        .into_iter()
        .filter(|p| p.8 != 0)
        .collect();
    if plans.is_empty() && products.is_empty() {
        if role >= roles::Role::Admin {
            // Global (seller_id = 0) namespace: admin/owner-only.
            plans = db.find_plans_by_name(Some(0), name);
            products = db
                .find_products_by_name(Some(0), name)
                .into_iter()
                .filter(|p| p.8 != 0)
                .collect();
        }
        if role == roles::Role::User {
            // Public-mode shop view: any seller's active entry except the
            // global namespace, which stays admin/owner-only.
            plans = db
                .find_plans_by_name(None, name)
                .into_iter()
                .filter(|p| p.6 != 0)
                .collect();
            products = db
                .find_products_by_name(None, name)
                .into_iter()
                .filter(|p| p.8 != 0 && p.1 != 0)
                .collect();
        }
        if role == roles::Role::Owner && plans.is_empty() && products.is_empty() {
            // Owner superuser: may resolve any seller's entry by name.
            plans = db.find_plans_by_name(None, name);
            products = db
                .find_products_by_name(None, name)
                .into_iter()
                .filter(|p| p.8 != 0)
                .collect();
        }
    }
    (plans, products)
}

/// Proof of control for channel binding on the bot side: the sender must be
/// an administrator of `channel` (creator counts), which also proves the bot
/// is a member of the chat — `getChatAdministrators` errors otherwise, which
/// this check treats the same as "not an admin". Timeout-safety: a single
/// request, no retry loops; the bot client's own request timeout bounds the
/// call.
async fn sender_admin_of_channel(bot: &Bot, channel: &str, sender_id: i64) -> bool {
    let Ok(members) = bot.get_chat_administrators(util::to_chat_id(channel)).await else {
        return false;
    };
    members.iter().any(|m| {
        m.user.id.0 as i64 == sender_id
            && matches!(
                m.kind,
                teloxide::types::ChatMemberKind::Owner(_)
                    | teloxide::types::ChatMemberKind::Administrator(_)
            )
    })
}

/// Shared non-admin validation for `/setchannel` and `/setup` channel
/// targets (proof of control, mirroring `POST /sellers/{id}/channel`):
/// positive numeric ids are private chats and can never be postable
/// channels, free-form strings are not resolvable targets, and `@handles`
/// require the sender to be an administrator of that chat. Negative numeric
/// ids pass (Telegram channels/groups are negative). Returns the refusal
/// message to send, or `None` when the target may be used.
async fn check_seller_channel_target(bot: &Bot, channel: &str, sender_id: i64) -> Option<String> {
    if let Ok(n) = channel.parse::<i64>() {
        if n >= 0 {
            return Some(
                "Channel id must be a group/channel id (negative) — private chats cannot be target channels."
                    .to_string(),
            );
        }
        return None;
    }
    if !channel.starts_with('@') {
        return Some("Use a @channelhandle (or a negative group/channel id).".to_string());
    }
    if sender_admin_of_channel(bot, channel, sender_id).await {
        None
    } else {
        Some(
            "You must be an admin of that channel, and the bot must be a member — add the bot first."
                .to_string(),
        )
    }
}

/// Resolve the target channel for a caller. Sellers may only operate on their
/// OWN channel (`seller_channel`); the global default fallback is reserved
/// for Admin/Owner, so a seller can never post/grant into a channel they do
/// not control via the default config.
fn channel_of(db: &Db, _cfg: &SharedConfig, seller_id: Option<i64>, is_admin: bool) -> String {
    if let Some(sid) = seller_id
        && let Some(c) = db.seller_channel(sid)
    {
        return c;
    }
    if !is_admin {
        return String::new();
    }
    db.get_config("channel").unwrap_or_default()
}

/// Resolve the target channel for a channel-scoped command, enforcing seller
/// tenancy (AUTO_ROUTE.md principle 2: every business entity is
/// seller-scoped). A seller may only target their OWN `seller_channel` —
/// never an arbitrary `@channel` or numeric id the bot happens to be a member
/// of. Admin/Owner keep the historical behavior: an explicit argument wins
/// verbatim, otherwise the `channel_of` fallback chain. On `Err` the payload
/// is a ready-to-send one-line reply for the caller.
fn resolve_target_channel(
    db: &Db,
    cfg: &SharedConfig,
    user_id: i64,
    is_admin: bool,
    explicit: Option<String>,
) -> Result<String, String> {
    if is_admin {
        return Ok(explicit.unwrap_or_else(|| channel_of(db, cfg, Some(user_id), true)));
    }
    let own = channel_of(db, cfg, Some(user_id), false);
    if own.is_empty() {
        return Err("No channel set. Use /setchannel @channel first.".to_string());
    }
    if let Some(ch) = explicit
        && ch != own
    {
        return Err("You can only target your own channel. Set it with /setchannel.".to_string());
    }
    Ok(own)
}

fn split_channel_arg(input: &str) -> (Option<String>, &str) {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return (None, trimmed);
    }
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let rest = parts.next().unwrap_or("").trim_start();
    let is_channel = first.starts_with('@') || first.parse::<i64>().map(|n| n < 0).unwrap_or(false);
    if is_channel {
        (Some(first.to_string()), rest)
    } else {
        (None, trimmed)
    }
}

fn parse_schedule_datetime(input: &str) -> Option<String> {
    use chrono::{DateTime, NaiveDateTime, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc).to_rfc3339());
    }
    let s = input.replace('T', " ");
    let fmts = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"];
    for fmt in fmts {
        if let Ok(nd) = NaiveDateTime::parse_from_str(&s, fmt) {
            return Some(nd.and_utc().to_rfc3339());
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        let nd = d.and_hms_opt(0, 0, 0).unwrap();
        return Some(nd.and_utc().to_rfc3339());
    }
    None
}

/// Strict slot-time validation (D4, v1.4): RFC3339, or the bot-friendly
/// "YYYY-MM-DD HH:MM" read as UTC — normalized to RFC3339 either way, since
/// `create_booking` parses the stored value with RFC3339 only. Unlike
/// `parse_schedule_datetime` this is deliberately strict: no date-only, no
/// seconds-only variants — a booking slot must be unambiguous. Past-ness is
/// checked by the caller (it needs `Utc::now()` anyway).
fn parse_slot_datetime(input: &str) -> Option<chrono::DateTime<Utc>> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(input) {
        return Some(dt.with_timezone(&Utc));
    }
    let naive = chrono::NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M").ok()?;
    Some(naive.and_utc())
}

/// `YYYY-MM-DD` rendering of an RFC3339 timestamp for bot-width lines
/// (`/mcptoken list`); unparseable input renders as an em-dash placeholder.
fn rfc3339_date(s: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| "—".to_string())
}

/// The console base URL for one-time setup links (D7) — `Some` only when
/// WEB_URL is explicitly configured (same check `Config::web_url` applies).
/// Without it the bot would DM a localhost link no seller can reach, so the
/// legacy plaintext-password DM stays the fallback until WEB_URL is set.
/// The raw setup token is NEVER logged — only the caller builds the URL.
fn web_base_url() -> Option<String> {
    match std::env::var("WEB_URL") {
        Ok(u) if !u.trim().is_empty() => Some(u.trim().trim_end_matches('/').to_string()),
        _ => None,
    }
}

/// Mint a one-time setup link for `account_id` and return the full claim URL
/// (`{web_base}/setup/claim?token={raw}`); `None` when WEB_URL is unset —
/// the caller falls back to the legacy password DM. The raw token exists
/// only inside the returned string (and the DM).
fn setup_claim_url(db: &Db, account_id: i64) -> Option<String> {
    let base = web_base_url()?;
    let raw = db.create_setup_link(account_id);
    Some(format!("{base}/setup/claim?token={raw}"))
}

/// Shared seller confirm/decline body (BOOKINGS_UX_SPEC §4.5/§4.6): the
/// booking is resolved against the CALLER's own scoped list only (a foreign
/// or unknown id is the same "not found" message — no oracle), the Db write
/// is seller-scoped, and the buyer DM is best-effort (logged, never fatal).
/// `confirm` picks the transition; sellers only — admins/owner are routed to
/// the console by the caller.
async fn booking_respond(
    bot: &Bot,
    db: &Db,
    chat_id: teloxide::types::ChatId,
    seller_id: i64,
    arg: &str,
    confirm: bool,
) -> HandlerResult {
    let key = arg.trim();
    let usage = if confirm {
        "Usage: /bookingconfirm <booking_id>"
    } else {
        "Usage: /bookingdecline <booking_id>"
    };
    if key.is_empty() {
        bot.send_message(chat_id, usage).await?;
        return Ok(());
    }
    // The id may be the integer row id or the public 8-char token — sellers
    // see the token in the console table and /bookings. Matching against the
    // seller's own scoped rows keeps foreign ids indistinguishable.
    let find = |rows: Vec<crate::db::BookingListRow>| {
        rows.into_iter().find(|(id, token, ..)| {
            *token == key || id.to_string() == key || format!("#{id}") == key
        })
    };
    let Some((id, _token, buyer, service, starts, _ends, _status, _expires, _sid)) =
        find(db.list_bookings_filtered(Some(seller_id), None, 200))
    else {
        bot.send_message(chat_id, "Booking not found or already closed.")
            .await?;
        return Ok(());
    };
    let target = if confirm { "confirmed" } else { "declined" };
    if !db.set_booking_status(id, Some(seller_id), target) {
        // The row was held/expired a moment ago, so a false here is a lost
        // race (slot re-taken on confirm) or a concurrent terminal
        // transition. Re-read the row to answer precisely instead of
        // guessing — still no oracle: only the caller's own row is read.
        let now_status = find(db.list_bookings_filtered(Some(seller_id), None, 200))
            .map(|(_, _, _, _, _, _, st, _, _)| st)
            .unwrap_or_else(|| "gone".to_string());
        match now_status.as_str() {
            "confirmed" | "declined" | "cancelled" => {
                bot.send_message(chat_id, format!("Booking #{id} is already {now_status}."))
                    .await?;
            }
            _ if confirm => {
                bot.send_message(
                    chat_id,
                    format!("Could not confirm #{id}: the slot is already taken."),
                )
                .await?;
            }
            _ => {
                bot.send_message(chat_id, "Booking not found or already closed.")
                    .await?;
            }
        }
        return Ok(());
    }
    // Notify the buyer FIRST, so the seller's success line ("The buyer has
    // been notified") is as truthful as the copy can be. A failed DM is
    // logged and never fails the command.
    let starts_shown = starts.as_deref().unwrap_or("—");
    let dm = if confirm {
        format!(
            "Booking #{id} confirmed — {service} at {starts_shown}.\nSeller: {}\nChange of plans? /cancelbooking #{id}",
            seller_display(db, seller_id)
        )
    } else {
        format!(
            "Booking #{id} ({service}) was declined by the seller. Find another time via /services."
        )
    };
    match bot.send_message(teloxide::types::ChatId(buyer), dm).await {
        Ok(_) => crate::metrics::bot_sent(),
        Err(e) => {
            tracing::warn!(
                "booking_respond: failed to DM buyer {} about booking #{}: {}",
                buyer,
                id,
                e
            )
        }
    }
    let reply = if confirm {
        format!("Booking #{id} confirmed. The buyer has been notified.")
    } else {
        format!("Booking #{id} declined. The buyer has been notified.")
    };
    bot.send_message(chat_id, reply).await?;
    Ok(())
}

/// Parse `/planadd name | price | asset | days` into
/// (name, price, asset, days); days must be in 1..=MAX_PLAN_DAYS.
pub(crate) fn parse_plan_add(input: &str) -> Result<(&str, &str, &str, i64), PlanAddError> {
    let parts: Vec<&str> = input.split('|').map(|s| s.trim()).collect();
    if parts.len() < 4 {
        return Err(PlanAddError::Usage);
    }
    match parts[3].parse::<i64>() {
        Ok(d) if (1..=MAX_PLAN_DAYS).contains(&d) => Ok((parts[0], parts[1], parts[2], d)),
        _ => Err(PlanAddError::Days(parts[3].to_string())),
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum PlanAddError {
    Usage,
    /// Raw days field that failed the 1..=MAX_PLAN_DAYS validation.
    Days(String),
}

/// Parse `/productadd name | price | asset | kind | delivery | [stock]` into
/// (name, price, asset, kind, delivery, stock); delivery/stock are None for
/// '-'/blank/omitted and stock must parse as i64 otherwise.
#[allow(clippy::type_complexity)] // mirrors the command's flat field list
pub(crate) fn parse_product_add(
    input: &str,
) -> Result<(&str, &str, &str, &str, Option<&str>, Option<i64>), ProductAddError> {
    let parts: Vec<&str> = input.split('|').map(|s| s.trim()).collect();
    if parts.len() < 5 || parts[0].is_empty() {
        return Err(ProductAddError::Usage);
    }
    let kind = parts[3];
    if !matches!(kind, "digital_link" | "digital_file" | "physical") {
        return Err(ProductAddError::Kind(kind.to_string()));
    }
    // Digital products must be deliverable at creation time: a payload-less
    // digital product would take money and then fail at delivery.
    let delivery = parts
        .get(4)
        .copied()
        .map(str::trim)
        .filter(|d| !d.is_empty() && *d != "-");
    if delivery.is_none() && kind != "physical" {
        return Err(ProductAddError::Delivery);
    }
    let stock = match parts.get(5).map(|s| s.trim()) {
        None | Some("") | Some("-") => None,
        Some(s) => s
            .parse::<i64>()
            .map(Some)
            .map_err(|_| ProductAddError::Stock(s.to_string()))?,
    };
    Ok((parts[0], parts[1], parts[2], kind, delivery, stock))
}

#[derive(Debug, PartialEq)]
pub(crate) enum ProductAddError {
    Usage,
    Kind(String),
    /// Digital kind given without a delivery payload.
    Delivery,
    Stock(String),
}

/// Parse `/orderset <order_id> <status> [tracking...]`, joining the tracking
/// words. The status whitelist itself is enforced by `Db::set_order_status`
/// (I/O) and is deliberately not checked here.
pub(crate) fn parse_order_set(input: &str) -> Option<(&str, &str, Option<String>)> {
    let parts: Vec<&str> = input.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let tracking = (parts.len() > 2).then(|| parts[2..].join(" "));
    Some((parts[0], parts[1], tracking))
}

/// Parse `/cartadd name [qty]`: if the last whitespace token is an integer,
/// treat it as qty; otherwise qty defaults to 1 and the whole input is the name.
pub(crate) fn parse_cart_add_arg(input: &str) -> (&str, i64) {
    let input = input.trim();
    if input.is_empty() {
        return ("", 1);
    }
    // Find last whitespace boundary.
    if let Some(pos) = input.rfind(char::is_whitespace) {
        let (head, tail) = input.split_at(pos);
        let tail = tail.trim();
        if let Ok(q) = tail.parse::<i64>() {
            let head = head.trim();
            if !head.is_empty() {
                return (head, q);
            }
        }
    }
    (input, 1)
}
/// numbers fall back silently (count -> 1, days -> None).
pub(crate) fn parse_gen_codes(input: &str) -> Option<(&str, usize, Option<i64>)> {
    let parts: Vec<&str> = input.split('|').map(|s| s.trim()).collect();
    if parts.is_empty() || parts[0].is_empty() {
        return None;
    }
    let count: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let days: Option<i64> = parts.get(2).and_then(|s| s.parse().ok());
    Some((parts[0], count, days))
}

/// Seller/admin plan listing (the historical format): one line per plan with
/// the (inactive) marker on retired rows.
fn plans_text(plans: Vec<crate::db::PlanRow>) -> String {
    let mut out = String::from("Plans:\n");
    for (name, price, asset, days, _ch, active, _seller) in plans {
        out.push_str(&format!(
            "- {} : {} {} / {} days {}\n",
            name,
            price,
            asset,
            days,
            if active { "" } else { "(inactive)" }
        ));
    }
    out
}

/// Seller display name for buyer-facing listings (P2#11): the seller's
/// Telegram username when the sellers row carries a usable one, else an
/// `@id:<user_id>` tag. The username form is exactly what
/// /buy <seller>/<name> accepts, so listings are directly actionable.
fn seller_display(db: &Db, seller_id: i64) -> String {
    match db
        .get_seller(seller_id)
        .and_then(|s| s.1)
        .map(|u| u.trim().trim_start_matches('@').to_string())
        .filter(|u| !u.is_empty())
    {
        Some(u) => format!("@{u}"),
        None => format!("@id:{seller_id}"),
    }
}

/// Buyer-facing invoice reply (P2#13): amount, the pay link as a tappable
/// URL button (when the provider returned a parsable one), and the neutral
/// /paid hint — the buyer never needs the seller's coaching line.
async fn send_invoice_created(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    label: &str,
    price: &str,
    asset: &str,
    pay_url: &str,
    inv_id: &str,
) -> HandlerResult {
    let mut text = format!("Invoice created for {label}.\nAmount: {price} {asset}\n");
    if !pay_url.is_empty() {
        text.push_str("Pay: ");
        text.push_str(pay_url);
        text.push('\n');
    }
    text.push_str(&format!(
        "You'll receive your item automatically after payment is verified — or run /paid {inv_id} to check now."
    ));
    let msg = bot.send_message(chat_id, text);
    // teloxide URL buttons take a parsed reqwest::Url; an unparsable pay link
    // degrades to the plain-text message (the URL is still shown as text).
    match pay_url
        .parse::<reqwest::Url>()
        .ok()
        .map(|u| InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::url("Pay now", u)]]))
    {
        Some(kb) => msg.reply_markup(kb).await?,
        None => msg.await?,
    };
    Ok(())
}

/// The buyer menu keyboard: Shop / Plans / Help (P2#10). The callbacks are
/// answered by callbacks.rs (`menu:*`), which replies role-aware too.
fn buyer_menu_kb() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("Shop", "menu:shop"),
            InlineKeyboardButton::callback("Plans", "menu:plans"),
        ],
        vec![InlineKeyboardButton::callback("Help", "menu:help")],
    ])
}

#[tracing::instrument(skip_all)]
async fn send_menu(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    role: roles::Role,
) -> HandlerResult {
    // Role-aware (P2#10): plain buyers get the shop surface; sellers/admins
    // keep the operator buttons.
    let kb = if role == roles::Role::User {
        buyer_menu_kb()
    } else {
        InlineKeyboardMarkup::new(vec![
            vec![
                InlineKeyboardButton::callback("Post", "menu:post"),
                InlineKeyboardButton::callback("Channel", "menu:channel"),
            ],
            vec![
                InlineKeyboardButton::callback("Crypto", "menu:crypto"),
                InlineKeyboardButton::callback("Help", "menu:help"),
            ],
        ])
    };
    bot.send_message(chat_id, "Menu:").reply_markup(kb).await?;
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn handle_crypto(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    db: &Db,
    cfg: &SharedConfig,
    crypto: Option<Arc<CryptoPay>>,
    user_id: i64,
    arg: &str,
) -> HandlerResult {
    let arg = arg.trim();
    let parts: Vec<&str> = arg.split_whitespace().collect();
    if parts.is_empty() {
        bot.send_message(chat_id, "Usage: /crypto <me|balance|rates|invoices [status]|invoice <amount> <asset> [desc]|setup <token>|check <amount> <asset>>").await?;
        return Ok(());
    }
    match parts[0] {
        "setup" => {
            if parts.len() < 2 {
                bot.send_message(chat_id, "Usage: /crypto setup <token>")
                    .await?;
            } else {
                db.set_config("crypto_token", parts[1]);
                bot.send_message(chat_id, "Crypto Pay token saved.").await?;
            }
        }
        "me" => {
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c.get_me().await {
                    Ok(v) => bot.send_message(chat_id, format!("App: {}", v)).await?,
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => { bot.send_message(chat_id, "No crypto token configured. Use /crypto setup <token> or set CRYPTO_PAY_TOKEN.").await? }
            };
        }
        "balance" => {
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c.get_balance().await {
                    Ok(v) => bot.send_message(chat_id, format!("Balance: {}", v)).await?,
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => {
                    bot.send_message(chat_id, "No crypto token configured.")
                        .await?
                }
            };
        }
        "rates" => {
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c.get_exchange_rates().await {
                    Ok(v) => bot.send_message(chat_id, format!("Rates: {}", v)).await?,
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => {
                    bot.send_message(chat_id, "No crypto token configured.")
                        .await?
                }
            };
        }
        "invoices" => {
            let status = parts.get(1).copied();
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c.get_invoices(status).await {
                    Ok(v) => {
                        bot.send_message(chat_id, format!("Invoices: {}", v))
                            .await?
                    }
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => {
                    bot.send_message(chat_id, "No crypto token configured.")
                        .await?
                }
            };
        }
        "invoice" => {
            // /crypto invoice <amount> <asset> [desc...]
            if parts.len() < 3 {
                bot.send_message(
                    chat_id,
                    "Usage: /crypto invoice <amount> <asset> [description]",
                )
                .await?;
                return Ok(());
            }
            let amount = parts[1];
            let asset = parts[2];
            let desc = if parts.len() > 3 {
                Some(parts[3..].join(" "))
            } else {
                None
            };
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c
                    .create_invoice(amount, asset, desc.as_deref(), Some("viewItem"), None)
                    .await
                {
                    Ok(v) => {
                        let inv_id = v
                            .get("invoice_id")
                            .map(|x| x.to_string())
                            .unwrap_or_default();
                        let pay_url = v.get("pay_url").and_then(|x| x.as_str()).unwrap_or("");
                        if !inv_id.is_empty() {
                            db.save_invoice(
                                &inv_id,
                                user_id,
                                amount,
                                asset,
                                desc.as_deref(),
                                "active",
                                Some(pay_url),
                                None,
                                // Bind the invoice to its creator so only that
                                // user (or Admin/Owner) can later complete it.
                                Some(user_id),
                                None,
                            );
                        }
                        bot.send_message(chat_id, format!("Invoice created:\n{}", v))
                            .await?
                    }
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => {
                    bot.send_message(chat_id, "No crypto token configured.")
                        .await?
                }
            };
        }
        "check" => {
            if parts.len() < 3 {
                bot.send_message(chat_id, "Usage: /crypto check <amount> <asset>")
                    .await?;
                return Ok(());
            }
            let cp = crypto_or_load(db, cfg, crypto);
            match cp {
                Some(c) => match c.create_check(parts[1], parts[2]).await {
                    Ok(v) => bot.send_message(chat_id, format!("Check: {}", v)).await?,
                    Err(e) => bot.send_message(chat_id, format!("Error: {}", e)).await?,
                },
                None => {
                    bot.send_message(chat_id, "No crypto token configured.")
                        .await?
                }
            };
        }
        _ => {
            bot.send_message(
                chat_id,
                "Unknown crypto subcommand. Use /crypto with no args for usage.",
            )
            .await?;
        }
    }
    Ok(())
}

fn crypto_or_load(
    db: &Db,
    cfg: &SharedConfig,
    crypto: Option<Arc<CryptoPay>>,
) -> Option<Arc<CryptoPay>> {
    if crypto.is_some() {
        return crypto;
    }
    let token = db
        .get_config("crypto_token")
        .or_else(|| cfg.crypto_token.clone());
    token.map(CryptoPay::new).map(Arc::new)
}

/// Role string used for audit rows, matching the REST handlers'
/// `session.role` vocabulary so bot and console approvals are
/// indistinguishable in the audit log.
fn audit_role_str(role: roles::Role) -> &'static str {
    match role {
        roles::Role::Owner => "owner",
        roles::Role::Admin => "admin",
        roles::Role::Seller => "seller",
        roles::Role::User => "user",
    }
}

/// `/refundapprove [refund_id]` — admin/owner approval of pending refunds.
/// Mirrors the REST state machine (`api::finance_refunds_approve`) exactly:
/// not-found reply, executed/invalid-state conflicts, approve only from
/// pending/approved, and the same `refund_approve` audit event with the same
/// JSON meta. With no id it lists up to 10 pending refunds. The bot never
/// moves money: the transfer itself stays on the console Finance page or the
/// REST execute endpoint.
async fn handle_refund_approve(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    db: &Db,
    user_id: i64,
    role: roles::Role,
    arg: &str,
) -> HandlerResult {
    if arg.is_empty() {
        let pending: Vec<_> = db
            .list_refunds(None)
            .into_iter()
            .filter(|r| r.7 == "pending")
            .take(10)
            .collect();
        if pending.is_empty() {
            bot.send_message(chat_id, "No pending refunds.").await?;
        } else {
            let mut out = String::from("Pending refunds:\n");
            for (id, seller, invoice, _user, amount, asset, _reason, _status) in pending {
                out.push_str(&format!(
                    "#{} | seller {} | {} {} | inv {}\n",
                    id,
                    seller,
                    amount,
                    asset,
                    invoice.as_deref().unwrap_or("-")
                ));
            }
            out.push_str("Approve with /refundapprove <id>.");
            bot.send_message(chat_id, out).await?;
        }
        return Ok(());
    }
    let id: i64 = match arg.parse() {
        Ok(v) => v,
        Err(_) => {
            bot.send_message(chat_id, "Usage: /refundapprove [refund_id]")
                .await?;
            return Ok(());
        }
    };
    let refund = match db.get_refund(id) {
        Some(r) => r,
        None => {
            bot.send_message(chat_id, format!("Refund #{} not found.", id))
                .await?;
            return Ok(());
        }
    };
    let (_rid, _seller, ref invoice_id, _user, ref amount, ref asset, _reason, ref status, ..) =
        refund;
    if status == "executed" {
        bot.send_message(chat_id, format!("Refund #{} is already executed.", id))
            .await?;
        return Ok(());
    }
    if status != "pending" && status != "approved" {
        bot.send_message(
            chat_id,
            format!("Cannot approve refund #{} in status '{}'.", id, status),
        )
        .await?;
        return Ok(());
    }
    let already_approved = status == "approved";
    db.set_refund_status(id, "approved");
    let meta = serde_json::json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
    })
    .to_string();
    db.log_action(
        user_id,
        audit_role_str(role),
        "refund_approve",
        "refund",
        id,
        Some(&meta),
    );
    if already_approved {
        bot.send_message(
            chat_id,
            format!(
                "Refund #{} was already approved. Approval recorded. Execute the transfer from the web console Finance page or REST (the bot does not move money).",
                id
            ),
        )
        .await?;
    } else {
        bot.send_message(
            chat_id,
            format!(
                "Refund #{} approved. Execute the transfer from the web console Finance page or REST (the bot does not move money).",
                id
            ),
        )
        .await?;
    }
    Ok(())
}

/// `/payoutapprove [payout_id]` — admin/owner approval of pending payouts.
/// Mirrors the REST state machine (`api::finance_payouts_approve`): not-found
/// reply, executed/invalid-state conflicts, approve only from
/// pending/approved, and the same `payout_approve` audit event. REST keeps the
/// two-person money rule structurally (owner-only gate); the bot gate is
/// admin+, so the rule is enforced explicitly here: nobody may approve a
/// payout that benefits their own seller account. The bot never transfers:
/// execution stays on the console Finance page or the REST execute endpoint.
async fn handle_payout_approve(
    bot: &Bot,
    chat_id: teloxide::types::ChatId,
    db: &Db,
    user_id: i64,
    role: roles::Role,
    arg: &str,
) -> HandlerResult {
    if arg.is_empty() {
        let pending: Vec<_> = db
            .list_payouts(None)
            .into_iter()
            .filter(|p| p.5 == "pending")
            .take(10)
            .collect();
        if pending.is_empty() {
            bot.send_message(chat_id, "No pending payouts.").await?;
        } else {
            let mut out = String::from("Pending payouts:\n");
            for (id, seller, amount, asset, destination, _status) in pending {
                let dest = destination.unwrap_or_default();
                let dest = if dest.is_empty() {
                    "-".to_string()
                } else if dest.chars().count() > 12 {
                    format!("{}…", dest.chars().take(10).collect::<String>())
                } else {
                    dest
                };
                out.push_str(&format!(
                    "#{} | seller {} | {} {} | to {}\n",
                    id, seller, amount, asset, dest
                ));
            }
            out.push_str("Approve with /payoutapprove <id>.");
            bot.send_message(chat_id, out).await?;
        }
        return Ok(());
    }
    let id: i64 = match arg.parse() {
        Ok(v) => v,
        Err(_) => {
            bot.send_message(chat_id, "Usage: /payoutapprove [payout_id]")
                .await?;
            return Ok(());
        }
    };
    let payout = match db.get_payout(id) {
        Some(p) => p,
        None => {
            bot.send_message(chat_id, format!("Payout #{} not found.", id))
                .await?;
            return Ok(());
        }
    };
    let (_pid, seller_id, ref amount, ref asset, ref destination, status, ..) = payout;
    // Two-person money rule: the beneficiary seller never approves their own
    // payout (REST enforces this via its owner-only gate; the bot's admin+
    // gate needs the explicit seller_id check — an admin account may also
    // carry a seller profile).
    if seller_id == user_id {
        bot.send_message(
            chat_id,
            "You may not approve a payout that benefits your own seller account.",
        )
        .await?;
        return Ok(());
    }
    if status == "paid" {
        bot.send_message(chat_id, format!("Payout #{} is already executed.", id))
            .await?;
        return Ok(());
    }
    if status != "pending" && status != "approved" {
        bot.send_message(
            chat_id,
            format!("Cannot approve payout #{} in status '{}'.", id, status),
        )
        .await?;
        return Ok(());
    }
    let already_approved = status == "approved";
    db.set_payout_status(id, "approved");
    let meta = serde_json::json!({
        "amount": amount, "asset": asset, "destination": destination,
    })
    .to_string();
    db.log_action(
        user_id,
        audit_role_str(role),
        "payout_approve",
        "payout",
        id,
        Some(&meta),
    );
    if already_approved {
        bot.send_message(
            chat_id,
            format!(
                "Payout #{} was already approved. Approval recorded. Execute the transfer from the web console Finance page or REST (the bot does not move money).",
                id
            ),
        )
        .await?;
    } else {
        bot.send_message(
            chat_id,
            format!(
                "Payout #{} approved. Execute the transfer from the web console Finance page or REST (the bot does not move money).",
                id
            ),
        )
        .await?;
    }
    Ok(())
}

#[tracing::instrument(skip_all)]
async fn setup_bot_profile(bot: &Bot) -> HandlerResult {
    use teloxide::types::BotCommand;
    let _cmds = vec![
        BotCommand::new("help", "Show help"),
        BotCommand::new("menu", "Main menu"),
        BotCommand::new("channel", "Show target channel"),
        BotCommand::new("setchannel", "Set target channel"),
        BotCommand::new("post", "Post text to channel"),
        BotCommand::new("postphoto", "Post a photo"),
        BotCommand::new("edit", "Edit posted message"),
        BotCommand::new("delete", "Delete posted message"),
        BotCommand::new("pin", "Pin a message"),
        BotCommand::new("unpin", "Unpin all"),
        BotCommand::new("invite", "Create invite link"),
        BotCommand::new("setdesc", "Set channel description"),
        BotCommand::new("settitle", "Set channel title"),
        BotCommand::new("info", "Channel info"),
        BotCommand::new("crypto", "Crypto Pay commands"),
        BotCommand::new("plans", "List subscription plans"),
        BotCommand::new("planadd", "Add a plan"),
        BotCommand::new("plandel", "Remove a plan"),
        BotCommand::new("productadd", "Add a product"),
        BotCommand::new("products", "List my products"),
        BotCommand::new("productdel", "Deactivate a product"),
        BotCommand::new("shop", "Browse the shop"),
        BotCommand::new("buy", "Buy a plan or product"),
        BotCommand::new("cart", "Show shopping cart"),
        BotCommand::new("cartadd", "Add item to cart"),
        BotCommand::new("cartremove", "Remove item from cart"),
        BotCommand::new("cartclear", "Clear cart"),
        BotCommand::new("checkout", "Checkout cart"),
        BotCommand::new("paid", "Claim payment"),
        BotCommand::new("redeem", "Redeem activation code"),
        BotCommand::new("gencodes", "Generate activation codes"),
        BotCommand::new("codes", "List activation codes"),
        BotCommand::new("grant", "Grant access"),
        BotCommand::new("revoke", "Revoke access"),
        BotCommand::new("subs", "List subscriptions"),
        BotCommand::new("expirecheck", "Run expiry cleanup"),
        BotCommand::new("broadcast", "Broadcast to all users"),
        BotCommand::new("setup", "Easy seller setup"),
        BotCommand::new("becomeseller", "Become a seller"),
        BotCommand::new("mybookings", "Your bookings"),
        BotCommand::new("cancelbooking", "Cancel your booking"),
        BotCommand::new("bookingconfirm", "Confirm a booking (sellers)"),
        BotCommand::new("bookingdecline", "Decline a booking (sellers)"),
        BotCommand::new("mcptoken", "Manage MCP API tokens (sellers)"),
        BotCommand::new("ai", "AI generate content"),
        BotCommand::new("aipost", "AI-assisted post"),
        BotCommand::new("refundapprove", "Approve a pending refund"),
        BotCommand::new("payoutapprove", "Approve a pending payout"),
        BotCommand::new("promocreate", "Create a promo code (sellers)"),
        BotCommand::new("promolist", "List promo codes"),
        BotCommand::new("promodel", "Revoke a promo code"),
        BotCommand::new("promo", "Apply a promo code"),
        BotCommand::new("exportdata", "Export buyer data (owner)"),
        BotCommand::new("deletedata", "Delete buyer data (owner)"),
    ];
    bot.set_my_description()
        .description(
            "Telegram shop: digital products, channel memberships, codes and bookings. Browse /shop — buy with /buy.",
        )
        .await?;
    bot.set_my_short_description()
        .short_description("Channel & crypto agent")
        .await?;
    Ok(())
}
#[tracing::instrument(skip_all)]
async fn grant_access(
    bot: &Bot,
    channel: &str,
    _user_id: i64,
    days: i64,
) -> Result<String, String> {
    let cid = util::to_chat_id(channel);
    // Checked expiry math: an extreme `days` value must degrade to an
    // immediately-expiring link, never panic the request path.
    let expire = Duration::try_days(days)
        .and_then(|d| Utc::now().checked_add_signed(d))
        .unwrap_or_else(Utc::now);
    match bot
        .create_chat_invite_link(cid)
        .member_limit(1)
        .expire_date(expire)
        .await
    {
        Ok(link) => Ok(link.invite_link),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests;
