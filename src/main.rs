use std::net::SocketAddr;
use std::sync::Arc;
use teloxide::{
    dispatching::dialogue::{ErasedStorage, SqliteStorage, Storage, serializer::Json},
    error_handlers::LoggingErrorHandler,
    prelude::*,
    update_listeners::Polling,
};
use tracing_subscriber::EnvFilter;

use daybook::{
    ai, api, bots, config,
    config::AppConfig,
    config::Config,
    crypto,
    db::Db,
    dialogue::State,
    expire_subscriptions, handlers,
    handlers::registry::{CommandContext, CommandRegistry},
    identity, mcp, metrics, roles, services, util, web,
};

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const POST_ATTEMPT_CAP: i64 = 10;

/// Best-effort owner DM from the primary bot. Send failures are logged (error
/// text only — never message content or secrets) and never propagate into a
/// money state transition.
async fn dm_owner(bm: &bots::BotManager, owner_id: i64, text: String) {
    if owner_id == 0 {
        return;
    }
    let bot = bm.for_seller(0).bot.clone();
    if let Err(e) = bot
        .send_message(teloxide::types::ChatId(owner_id), text)
        .await
    {
        tracing::warn!("money alert: failed to DM owner: {}", e);
    }
}

/// Emit an audited BLOCKING owner alert at most once per money row. The audit
/// is the durable "already alerted" marker, so a restart cannot re-spam.
async fn alert_once(
    db: &Db,
    bm: &bots::BotManager,
    owner_id: i64,
    action: &str,
    target_type: &str,
    target_id: &str,
    text: String,
) {
    if crate::handlers::money::has_audit_action(db, action, target_type, target_id) {
        return;
    }
    db.log_action(0, "system", action, target_type, target_id, None);
    dm_owner(bm, owner_id, text).await;
}

/// §3.3 exactly-once grant claim replay for a `paid_ungranted` invoice.
/// Returns true only when the durable grant (product_grant or subscription)
/// exists for the invoice. No invite-link or DM side effects here — the
/// claim row is the source of truth; the 24h/72h timers keep running from
/// `paid_ungranted_at` regardless of retries.
async fn retry_ungranted_grant(
    db: &Db,
    inv_id: &str,
    seller_id: i64,
    user_id: Option<i64>,
) -> bool {
    if let Some(pid) = db.invoice_product(inv_id) {
        if db.product_grant_exists(inv_id) {
            return true;
        }
        let Some(uid) = user_id.filter(|u| *u != 0) else {
            return false;
        };
        return db.grant_product(seller_id, uid, pid, inv_id).is_ok();
    }
    if db.subscription_exists_for_invoice(inv_id) {
        return true;
    }
    let Some(uid) = user_id.filter(|u| *u != 0) else {
        return false;
    };
    let plan_name = db.get_invoice(inv_id).and_then(|i| i.4);
    let plan_info = plan_name.as_ref().and_then(|pn| db.get_plan(seller_id, pn));
    let days = plan_info
        .as_ref()
        .map(|p| p.4)
        .unwrap_or(30)
        .clamp(1, crate::handlers::commands::MAX_PLAN_DAYS);
    let ch = plan_info
        .as_ref()
        .and_then(|p| p.5.clone())
        .or_else(|| db.seller_channel(seller_id))
        .or_else(|| db.get_config("channel"))
        .unwrap_or_default();
    if ch.is_empty() {
        return false;
    }
    db.create_subscription(seller_id, uid, None, &ch, days, Some(inv_id), None);
    db.subscription_exists_for_invoice(inv_id)
}

/// Route one scheduled post whose stored `scheduled_at` cannot parse as
/// RFC3339 into the existing bounded terminal-failed path. The due queue
/// compares the column lexicographically, so such a row either fires at the
/// wrong time (the string sorts below an RFC3339 timestamp) or sits pending
/// forever (it sorts above — e.g. "tomorrow" > any "2026-…"). The condition
/// is permanent (not transient), so every attempt logs at error with the row
/// id and the raw stored string; at the cap the row goes terminal-failed and
/// leaves the pending queue for good.
fn bump_unparseable_scheduled(db: &Db, id: i64, raw: &str) {
    let attempts = db.bump_scheduled_attempts(id);
    if attempts >= POST_ATTEMPT_CAP {
        db.mark_scheduled_failed(id);
        tracing::error!(
            "scheduled post {} failed terminally: stored scheduled_at {:?} is not RFC3339 and can never fire reliably ({} attempts); reschedule the post",
            id,
            raw,
            attempts
        );
    } else {
        tracing::error!(
            "scheduled post {} cannot fire: stored scheduled_at {:?} is not RFC3339 (attempt {}/{}); will terminal-fail at the cap",
            id,
            raw,
            attempts,
            POST_ATTEMPT_CAP
        );
    }
}

/// Security headers on every response of the merged router (security). All values
/// are static, so `HeaderValue::from_static` is valid for each. Applied
/// outside the request-timeout layer so aborted requests still carry them;
/// the metrics layer stays outermost so those responses are still counted.
async fn security_headers(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{HeaderName, HeaderValue, header};
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("camera=(), microphone=(), geolocation=()"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    );
    resp
}

fn main() {
    // P1 stack-overflow mitigation (2026-11-01): the binary aborted twice in
    // production with `thread 'tokio-rt-worker' has overflowed its stack /
    // fatal runtime error: stack overflow` while LIVE bot pollers dispatched
    // real Telegram updates (crash #1 minutes after the first live boot,
    // crash #2 after ~1h; tokenless setup boots ran for days with no crash).
    // A bare `#[tokio::main]` leaves worker threads on tokio's default 2 MiB
    // stack — far below the process `ulimit -s` (8 MiB) — and the live-update
    // dispatch path is the deepest frame chain in the process: every update
    // (including MyChatMember fall-throughs, which traverse the entire dptree
    // branch tree without reaching an endpoint) walks a chain of nested
    // matcher frames while teloxide moves the large Update/Message enums by
    // value. 16 MiB per worker (and per blocking-pool thread, which also
    // inherits this knob) is the standard, documented fix for dptree dispatch
    // depth. Worker-thread COUNT and all runtime semantics are unchanged from
    // the previous #[tokio::main] (multi_thread flavor, enable_all, default
    // worker_threads = num_cpus); only the per-thread stack grows.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(16 * 1024 * 1024)
        .build()
        .expect("failed to build tokio runtime"); // allowed: startup path
    runtime.block_on(run());
}

async fn run() {
    config::load_dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
    // Pin the metrics uptime gauge to process start (server observability).
    metrics::note_process_start();

    let app_cfg = AppConfig::from_env();
    let cfg: Config = app_cfg.shared.clone();
    // Fail fast on a malformed MASTER_KEY, and state plainly whether
    // secret-at-rest encryption is active. No key material is logged.
    match config::validate_master_key_at_startup() {
        Ok(active) => {
            if active {
                tracing::info!("secret encryption at rest: active (MASTER_KEY configured)");
            } else {
                tracing::info!(
                    "secret encryption at rest: inactive (MASTER_KEY unset; secret writes are refused)"
                );
            }
        }
        Err(e) => {
            eprintln!("startup aborted: {e}");
            std::process::exit(2);
        }
    }
    // CLI recovery subcommand: `daybook reset-owner ...`. Handled
    // before any server/bot startup so it works in setup mode (missing
    // bot token / .env). Exits the process when invoked.
    {
        let args: Vec<String> = std::env::args().collect();
        let cmd_idx = if args.get(1).map(String::as_str) == Some("reset-owner") {
            Some(1usize)
        } else if args.first().map(String::as_str) == Some("reset-owner") {
            Some(0usize)
        } else {
            None
        };
        if let Some(ci) = cmd_idx {
            // parse flags
            let mut print_claim = false;
            let mut delete_owner = false;
            let mut username: Option<String> = None;
            let mut password: Option<String> = None;
            let mut i = ci + 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--print-claim-code" => {
                        print_claim = true;
                        i += 1;
                    }
                    "--delete-owner" => {
                        delete_owner = true;
                        i += 1;
                    }
                    "--username" => {
                        if i + 1 < args.len() {
                            username = Some(args[i + 1].clone());
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    "--password" => {
                        if i + 1 < args.len() {
                            password = Some(args[i + 1].clone());
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }

            let db = match Db::new(&cfg.db_path) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("DB error: {}", e);
                    std::process::exit(1);
                }
            };

            if delete_owner {
                let code = db.delete_owners_and_mint_claim_code();
                db.log_action(
                    0,
                    "system",
                    "owner_reset",
                    "owner",
                    "all",
                    Some("delete-owner"),
                );
                eprintln!("{}", code);
                std::process::exit(0);
            }

            if let Some(uname) = username {
                let pw = match password {
                    Some(p) if !p.is_empty() => p,
                    _ => {
                        let g = util::gen_token(16);
                        eprintln!("{}", g);
                        g
                    }
                };
                match db.reset_owner_account(&uname, &pw).await {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("reset failed: {}", e);
                        std::process::exit(1);
                    }
                }
                db.log_action(
                    0,
                    "system",
                    "owner_reset",
                    "owner",
                    &uname,
                    Some("password_reset"),
                );
                std::process::exit(0);
            }

            if print_claim {
                let code = if db.owner_account_exists() {
                    db.claim_code()
                } else {
                    db.ensure_claim_code()
                };
                match code {
                    Some(c) => eprintln!("{}", c),
                    None => eprintln!("No claim code available."),
                }
                std::process::exit(0);
            }

            // No recognized mode.
            eprintln!(
                "Usage: daybook reset-owner [--print-claim-code] [--delete-owner] [--username X --password Y]"
            );
            std::process::exit(2);
        }
    }

    if !app_cfg.is_configured() {
        eprintln!("Setup mode: open the web UI to configure (bot token / owner id missing).");
    }

    let db = match Db::new(&cfg.db_path) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            tracing::error!("DB error: {e}; exiting so systemd can restart");
            // Must be non-zero: a silent `return` left systemd's
            // Restart=on-failure blind to a dead-on-arrival process.
            std::process::exit(1);
        }
    };

    // Backfill bots table once (when empty) from configured bots.
    db.backfill_bots_at_startup(&app_cfg.bots);

    // Seed owner web account (idempotent) only when configured.
    if app_cfg.is_configured() {
        {
            let uname = cfg.web_username();
            // Never fall back to the (public) owner id as the password: when
            // WEB_PASSWORD is unset or empty, generate a random secret and
            // surface it once on stderr so the operator can log in.
            let pw = std::env::var("WEB_PASSWORD")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| {
                    let pw = util::gen_token(16);
                    eprintln!("Generated web dashboard password (WEB_PASSWORD was unset): {pw}");
                    pw
                });
            if db.get_web_account_by_username(&uname).is_none()
                && let Err(e) = db.create_web_account(&uname, &pw, "owner", None).await
            {
                tracing::warn!("failed to seed owner web account: {}", e);
            }
        }
    }

    // Ensure default channel from env (primary bot) is stored if not set.
    let primary_default_channel = app_cfg.bots.first().and_then(|b| b.default_channel.clone());
    if db.get_config("channel").is_none()
        && let Some(ch) = &primary_default_channel
    {
        db.set_config("channel", ch);
    }

    // v1.3.x claim flow: setup is skippable, so an instance may run with no
    // owner web account. Until one exists (and setup is complete), maintain
    // the one-time claim code and print it once, loudly — the same
    // CSPRNG-secret-printed-once pattern as the generated WEB_PASSWORD above
    // (stderr at startup; the operator copies it from the console). Whenever
    // an owner account exists the code is stale and cleared: claim mode can
    // never return. Web claim handlers (web.rs) compare what the browser
    // presents against `db.claim_code()` and waive the code for loopback
    // peers — see the ConnectInfo note on the serve call below.
    if let Some(code) = db.ensure_claim_code() {
        eprintln!(
            "First-run claim code: {code}\n  One-time code for claiming the owner account in the web console (required only from a non-loopback browser; localhost needs no code)."
        );
    }

    let crypto = cfg
        .crypto_token
        .clone()
        .map(crypto::CryptoPay::new)
        .map(Arc::new);

    let ai_service: Option<Arc<ai::AiService>> = if cfg.ai_enabled {
        let provider: Option<Arc<dyn ai::AiProvider>> = match cfg.ai_api_style.as_str() {
            "anthropic" => {
                let key = cfg.ai_api_key.clone().unwrap_or_default();
                let base = cfg.ai_base_url.clone();
                match ai::anthropic::AnthropicProvider::new(base, key) {
                    Ok(p) => Some(Arc::new(p) as Arc<dyn ai::AiProvider>),
                    Err(e) => {
                        eprintln!("failed to build anthropic provider: {}", e);
                        None
                    }
                }
            }
            _ => {
                let key = cfg.ai_api_key.clone().unwrap_or_default();
                match ai::openai::OpenAiProvider::new(cfg.ai_base_url.clone(), key) {
                    Ok(p) => Some(Arc::new(p) as Arc<dyn ai::AiProvider>),
                    Err(e) => {
                        eprintln!("failed to build openai provider: {}", e);
                        None
                    }
                }
            }
        };
        provider.map(|p| {
            Arc::new(ai::AiService {
                provider: p,
                model: cfg.ai_model.clone(),
                db: db.clone(),
                rate_limiter: ai::rate_limit::RateLimiter::new(20),
                daily_token_budget_global: cfg.ai_daily_token_budget,
                enabled: true,
            })
        })
    } else {
        None
    };

    // Build BotContexts from app_cfg (enabled bots only). One hardened HTTP
    // client is shared by every bot (`Bot::new` would otherwise give each its
    // own client with teloxide's 17s default total timeout — under the 30s
    // long poll below it must be our 45s budget, see bots::bot_http_client).
    let bot_http = bots::bot_http_client();
    let mut bot_contexts: Vec<Arc<bots::BotContext>> = Vec::new();
    for bc in &app_cfg.bots {
        // A skip must never be silent: an operator with a dead bot otherwise
        // sees a running process that simply never polls. The env-var NAME is
        // logged (non-secret config metadata); never the token value.
        if bc.token.trim().is_empty() {
            tracing::error!(
                bot = %bc.key,
                token_env = ?bc.token_env,
                "bot '{}' skipped at startup: token env var {} is unset or empty — that bot will not poll until it is set (restart required)",
                bc.key,
                bc.token_env.as_deref().unwrap_or("<inline token entry>")
            );
            continue;
        }
        if bc.owner_id == 0 {
            tracing::error!(
                bot = %bc.key,
                token_env = ?bc.token_env,
                "bot '{}' skipped at startup: owner_id is unset (0) — set owner_id in the bot config and restart",
                bc.key
            );
            continue;
        }
        let bot_id = db.create_bot(&bc.key, bc.owner_id, None, bc.name.trim());
        if let Some(rec) = db.get_bot(bot_id)
            && !rec.enabled
        {
            tracing::warn!(
                bot = %bc.key,
                "bot '{}' skipped at startup: disabled in the console (enable it on the Bots page and restart)",
                bc.key
            );
            continue;
        }
        let ctx = Arc::new(bots::BotContext {
            id: bot_id,
            key: bc.key.clone(),
            bot: Bot::with_client(bc.token.clone(), bot_http.clone()),
            owner_id: bc.owner_id,
            shared: Arc::new(cfg.clone()),
            db: db.clone(),
        });
        bot_contexts.push(ctx);
    }
    if bot_contexts.is_empty() {
        tracing::error!(
            "no bots started: {} configured, 0 started — every bot was skipped (missing token env var, unset owner_id, or disabled); bot polling is DEAD until fixed and the process restarted",
            app_cfg.bots.len()
        );
    }

    let primary_ctx: Arc<bots::BotContext> = if let Some(first) = bot_contexts.first().cloned() {
        first
    } else {
        Arc::new(bots::BotContext {
            id: 0,
            key: "primary".to_string(),
            bot: Bot::with_client("0:setup-placeholder".to_string(), bot_http),
            owner_id: 0,
            shared: Arc::new(cfg.clone()),
            db: db.clone(),
        })
    };
    let bot = primary_ctx.bot.clone();
    // Personal-account identity actor (user-identity design Phase 1):
    // spawned alongside the bot dispatchers; every caller shares the
    // cloneable handle. Session material stays inside the task. The db is
    // the channel-allowlist source (design decision 5).
    let identity = identity::IdentityManager::spawn(&cfg, &config::data_dir(), db.clone());
    let bot_manager: Arc<bots::BotManager> = Arc::new(bots::BotManager {
        bots: bot_contexts.clone(),
        primary: primary_ctx.clone(),
        db: db.clone(),
        identity: identity.clone(),
    });

    // Dialogue storage (persistent).
    let dialogue_path = std::path::Path::new(&cfg.db_path)
        .parent()
        .map(|p| p.join("dialogue.db"))
        .unwrap_or_else(|| std::path::PathBuf::from("./data/dialogue.db"));
    if let Some(parent) = dialogue_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let storage: Arc<ErasedStorage<State>> =
        SqliteStorage::open(dialogue_path.to_str().unwrap(), Json)
            .await
            .expect("open dialogue storage")
            .erase();

    // Auto-selling background processor: verifies pending invoices and grants
    // access. pending_invoices() feeds the queue: every grantable 'active'
    // invoice (plan or product; webhook events enqueue via 'active') plus
    // 'granting' rows stale past the wedged-grant threshold (crash recovery).
    // Each invoice is claimed ('active' -> 'granting') before verification so
    // concurrent granters (this loop, /paid, the claim API) can never
    // double-grant; losers simply skip. Nothing is ever granted without a
    // verified 'paid' from the provider (fail-closed): a not-settled claim is
    // released back to 'active', an unresolvable claim (no token, no channel)
    // either ages out to terminal 'failed' at the attempt cap or is retried
    // once stale.
    if !bot_contexts.is_empty() {
        for bot_ctx in bot_contexts.clone() {
            let bot_bg = bot_ctx.bot.clone();
            let db_bg = db.clone();
            let cfg_bg = cfg.clone();
            let crypto_bg = crypto.clone();
            let bm = bot_manager.clone();
            let this_id = bot_ctx.id;
            tokio::spawn(async move {
                use std::time::Duration;
                loop {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    let pending = db_bg.pending_invoices();
                    for (inv_id, plan_name, uid, inv_seller) in pending {
                        if bm.for_seller(inv_seller).id != this_id {
                            continue;
                        }
                        // Resolve the crypto provider once per cycle; the loop's
                        // seller binding above is the only routing decision here.
                        let cp_local = crypto_bg.clone().or_else(|| {
                            db_bg
                                .get_config("crypto_token")
                                .or_else(|| cfg_bg.crypto_token.clone())
                                .map(crypto::CryptoPay::new)
                                .map(std::sync::Arc::new)
                        });
                        services::grant::grant_one(
                            &db_bg,
                            &bot_bg,
                            cp_local.as_deref(),
                            &inv_id,
                            plan_name.as_deref(),
                            uid,
                            inv_seller,
                            this_id,
                        )
                        .await;
                    }
                }
            });
        }
    }
    // Scheduled posts background processor: the sending bot is resolved per due row from the target channel (BotManager::for_channel, multi-bot design §7) — never a single primary sender, and never from the row's seller_id (owner-scheduled posts carry seller_id 0, which would resolve the primary bot for another bot's channel).
    {
        let bm_bg = bot_manager.clone();
        let db_bg = db.clone();
        tokio::spawn(async move {
            use std::time::Duration;
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                // Unparseable scheduled_at sweep: the due query compares the
                // stored string lexicographically, so a non-RFC3339 value
                // either fires early or sits pending forever. Every pending
                // poison row goes through the bounded terminal-failed path
                // (bump → cap → 'failed') and is skipped below — never sent.
                let mut poison_ids = std::collections::HashSet::new();
                for (id, raw) in db_bg.list_pending_scheduled_unparseable() {
                    bump_unparseable_scheduled(&db_bg, id, &raw);
                    poison_ids.insert(id);
                }
                let due = db_bg.list_due_scheduled();
                for (
                    id,
                    seller_id,
                    chat_id,
                    content,
                    kind,
                    _scheduled_at,
                    _status,
                    _created_at,
                    _posted,
                ) in due
                {
                    // Poison rows were already routed toward terminal-failed
                    // by the sweep above (bumped once this cycle); skip them
                    // silently here so they are never sent. The parse check
                    // covers a poison row inserted after the sweep ran.
                    if poison_ids.contains(&id)
                        || chrono::DateTime::parse_from_rfc3339(&_scheduled_at).is_err()
                    {
                        if !poison_ids.contains(&id) {
                            bump_unparseable_scheduled(&db_bg, id, &_scheduled_at);
                        }
                        continue;
                    }
                    let text = content.clone().unwrap_or_default();
                    // Resolve the sender from the authoritative target channel
                    // (channels.bot_id, then the channel's seller, then
                    // primary): owner-scheduled posts carry seller_id 0, so
                    // seller-based resolution would pick the primary bot for
                    // a channel owned by another bot.
                    let bot_ctx = bm_bg.for_channel(&chat_id);
                    let bot = bot_ctx.bot.clone();
                    // security outbound gate before this raw-Bot scheduled send.
                    // Seller scope is the row's own seller (owner-scheduled
                    // posts carry 0 → global floor). A held post stays due so
                    // a later policy change can release it; the gate already
                    // wrote an opaque audit row and a scoped warn log.
                    if crate::handlers::moderation::gate_outbound(
                        &db_bg,
                        seller_id,
                        Some(bot_ctx.id),
                        &text,
                    )
                    .is_err()
                    {
                        continue;
                    }
                    match bot
                        .send_message(util::to_chat_id(&chat_id), text.clone())
                        .await
                    {
                        Ok(sent) => {
                            let _ = db_bg.mark_scheduled_posted(id, sent.id.0);
                            let _ = db_bg.log_post(
                                seller_id,
                                &chat_id,
                                sent.id.0,
                                &kind,
                                Some(&text),
                                "bot",
                            );
                            tracing::info!("scheduled post {} posted", id);
                            // Metrics hook only (server): bot_messages_sent_total.
                            metrics::bot_sent();
                        }
                        Err(e) => {
                            // Bounded retry (POST_ATTEMPT_CAP): at the cap
                            // the post goes terminal-failed and leaves the
                            // due queue instead of retrying forever. Send
                            // failures are matched, never unwrapped — a
                            // broken chat must not panic the worker.
                            let attempts = db_bg.bump_scheduled_attempts(id);
                            if attempts >= POST_ATTEMPT_CAP {
                                db_bg.mark_scheduled_failed(id);
                                tracing::error!(
                                    "scheduled post {} failed terminally after {} send attempts: {}",
                                    id,
                                    attempts,
                                    e
                                );
                            } else {
                                tracing::warn!(
                                    "failed to post scheduled {} (attempt {}/{}): {}",
                                    id,
                                    attempts,
                                    POST_ATTEMPT_CAP,
                                    e
                                );
                            }
                        }
                    }
                }
            }
        });
    }
    // Expiry sweeper: periodically revoke expired subscriptions (kick + mark)
    // so lapsed members lose access even if nobody runs /expirecheck. The kick
    // and the notice both go through the bot that owns each row's seller
    // (multi-bot design §7), so bot A never kicks/DMs for bot B; a failed DM
    // is logged and never propagated — the sweeper must not die on a send
    // error.
    {
        let bm_bg = bot_manager.clone();
        let db_bg = db.clone();
        tokio::spawn(async move {
            use std::time::Duration;
            loop {
                tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
                let expired = expire_subscriptions(&bm_bg, &db_bg).await;
                for (_sub_id, seller_id, uid, channel) in &expired {
                    let bot_ctx = bm_bg.for_seller(*seller_id);
                    let bot = bot_ctx.bot.clone();
                    let notice = format!(
                        "Your access to {channel} has expired. To renew, use /plans or contact the seller."
                    );
                    // security outbound gate before this raw-Bot DM. The expiry
                    // transition is already durable; only the notice is
                    // skipped while held.
                    if crate::handlers::moderation::gate_outbound(
                        &db_bg,
                        *seller_id,
                        Some(bot_ctx.id),
                        &notice,
                    )
                    .is_ok()
                    {
                        match bot
                            .send_message(teloxide::types::ChatId(*uid), notice)
                            .await
                        {
                            Ok(_) => metrics::bot_sent(),
                            Err(e) => tracing::warn!(
                                "expiry sweeper: failed to DM user {} about expired access to {}: {}",
                                uid,
                                channel,
                                e
                            ),
                        }
                    }
                }
                if !expired.is_empty() {
                    tracing::info!("expiry sweeper revoked {} subscriptions", expired.len());
                }
            }
        });
    }
    // Held-slot sweeper: release booking holds that expired (D5) and DM each
    // buyer whose request expired — the status transition inside
    // expire_held_bookings is the exactly-once guard for the DM. The DM goes
    // through the bot that owns the row's seller (multi-bot design §7), so a
    // buyer is never messaged by another seller's bot. Mirrors the
    // subscription sweeper above: a failed DM is logged, never propagated,
    // and sellers are never DM'd on expiry (noise control).
    {
        let bm_bg = bot_manager.clone();
        let db_bg = db.clone();
        tokio::spawn(async move {
            use std::time::Duration;
            loop {
                tokio::time::sleep(Duration::from_secs(5 * 60)).await;
                let expired = db_bg.expire_held_bookings();
                for (booking_id, buyer, service, seller_id) in &expired {
                    let bot_ctx = bm_bg.for_seller(*seller_id);
                    let bot = bot_ctx.bot.clone();
                    let notice = format!(
                        "Booking #{booking_id} ({service}) expired — the seller didn't respond in time, so the slot was released.\nYou can book again: /book {service}"
                    );
                    // security outbound gate before this raw-Bot DM. The hold
                    // release is already durable; only the notice is skipped
                    // while held.
                    if crate::handlers::moderation::gate_outbound(
                        &db_bg,
                        *seller_id,
                        Some(bot_ctx.id),
                        &notice,
                    )
                    .is_ok()
                    {
                        match bot
                            .send_message(teloxide::types::ChatId(*buyer), notice)
                            .await
                        {
                            Ok(_) => metrics::bot_sent(),
                            Err(e) => tracing::warn!(
                                "held-slot sweeper: failed to DM buyer {} about expired booking #{}: {}",
                                buyer,
                                booking_id,
                                e
                            ),
                        }
                    }
                }
                if !expired.is_empty() {
                    tracing::info!("held-slot sweeper expired {} bookings", expired.len());
                }
            }
        });
    }
    // §3 paid-but-ungranted recovery. One 30s loop drives the injected-clock
    // timeline: 30s exactly-once grant retry, 24h system auto-refund + owner
    // and buyer alerts, 72h BLOCKING owner escalation / payout hold. A paid
    // invoice is NEVER moved to terminal 'failed' here. All timers are
    // internal operational targets, not customer SLAs (G7).
    {
        let bm_bg = bot_manager.clone();
        let db_bg = db.clone();
        let owner_id = cfg.owner_id;
        tokio::spawn(async move {
            use std::time::Duration;
            loop {
                tokio::time::sleep(Duration::from_secs(
                    daybook::db::PAID_UNGRANTED_RETRY_SECONDS as u64,
                ))
                .await;
                let now = chrono::Utc::now();
                let retry_cutoff = handlers::money::paid_ungranted_retry_cutoff(now);
                let auto_cutoff = handlers::money::paid_ungranted_auto_refund_cutoff(now);
                let escalation_cutoff = handlers::money::paid_ungranted_escalation_cutoff(now);
                let due = db_bg.paid_ungranted_before(&retry_cutoff);
                for (inv_id, seller_id, amount, asset, since) in due {
                    // 72h: blocking escalation, emitted once per invoice.
                    if since <= escalation_cutoff
                        && db_bg
                            .unresolved_ungranted_invoices(seller_id, &escalation_cutoff)
                            .iter()
                            .any(|id| id == &inv_id)
                    {
                        alert_once(
                            &db_bg,
                            &bm_bg,
                            owner_id,
                            "invoice_ungranted_escalation",
                            "invoice",
                            &inv_id,
                            format!(
                                "BLOCKING: invoice {inv_id} (seller {seller_id}, {amount} {asset}) is still ungranted past 72h with no executed refund. New payouts for this seller are held."
                            ),
                        )
                        .await;
                    }
                    // 24h: a system refund suppresses any further grant retry
                    // and stays subject to the owner-only approval flow.
                    if since <= auto_cutoff {
                        let idem = format!("auto:ungranted:{inv_id}");
                        let created = db_bg.create_refund_guarded(daybook::db::RefundInput {
                            seller_id,
                            invoice_id: Some(&inv_id),
                            user_id: None,
                            amount: &amount,
                            asset: &asset,
                            reason: Some("automatic refund: invoice paid but ungranted for 24h"),
                            idem_key: &idem,
                            created_by: -1,
                            created_role: "system",
                        });
                        match created {
                            Ok(c) if c.created_new => {
                                let meta = serde_json::json!({
                                    "seller_id": seller_id, "amount": amount,
                                    "asset": asset, "idem_key": idem,
                                })
                                .to_string();
                                db_bg.log_action(
                                    -1,
                                    "system",
                                    "invoice_ungranted_refund_queued",
                                    "invoice",
                                    &inv_id,
                                    Some(&meta),
                                );
                                tracing::warn!(
                                    "invoice {} ungranted past 24h: auto-refund #{} queued",
                                    inv_id,
                                    c.id
                                );
                                alert_once(
                                    &db_bg,
                                    &bm_bg,
                                    owner_id,
                                    "invoice_ungranted_refund_owner_notice",
                                    "invoice",
                                    &inv_id,
                                    format!(
                                        "Invoice {inv_id} ({amount} {asset}) paid but ungranted for 24h: refund #{} queued — approve in Finance.",
                                        c.id
                                    ),
                                )
                                .await;
                                // Buyer gets a "queued" notice only; "sent"
                                // comes after the owner executes the refund.
                                if let Some(uid) = db_bg.get_invoice(&inv_id).and_then(|i| i.5) {
                                    let bot_ctx = bm_bg.for_seller(seller_id);
                                    let notice = format!(
                                        "Your payment for invoice {inv_id} is being refunded ({amount} {asset})."
                                    );
                                    if handlers::moderation::gate_outbound(
                                        &db_bg,
                                        seller_id,
                                        Some(bot_ctx.id),
                                        &notice,
                                    )
                                    .is_ok()
                                    {
                                        let _ = bot_ctx
                                            .bot
                                            .send_message(teloxide::types::ChatId(uid), notice)
                                            .await;
                                    }
                                }
                            }
                            // Replayed key or a manual refund already in
                            // flight: suppressed, no second money event.
                            Ok(_) | Err(daybook::db::RefundCreateError::InFlight) => {}
                            Err(e) => tracing::warn!(
                                "invoice {}: auto-refund could not be queued: {:?}",
                                inv_id,
                                e
                            ),
                        }
                        continue;
                    }
                    // 30s: replay the exactly-once grant claim. Each attempt is
                    // audited; success returns the row to 'paid' and stops the
                    // job for that invoice. The 24h clock is never reset.
                    let user_id = db_bg.get_invoice(&inv_id).and_then(|i| i.5);
                    let granted = retry_ungranted_grant(&db_bg, &inv_id, seller_id, user_id).await;
                    if granted && db_bg.finalize_invoice_grant_retry(&inv_id) {
                        db_bg.log_action(
                            0,
                            "system",
                            "invoice_grant_retry",
                            "invoice",
                            &inv_id,
                            Some("outcome=granted"),
                        );
                        tracing::info!("invoice {} grant retry succeeded", inv_id);
                    } else {
                        db_bg.log_action(
                            0,
                            "system",
                            "invoice_grant_retry",
                            "invoice",
                            &inv_id,
                            Some("outcome=no_grant"),
                        );
                    }
                }
            }
        });
    }
    // §5 executing-row state machine + 09:00 UTC owner digest. The same
    // injectable-clock cutoffs drive the 30min transition and the 24h overdue
    // alert; no transfer is attempted and no state is guessed by this job.
    {
        let db_bg = db.clone();
        let bm_bg = bot_manager.clone();
        let owner_id = cfg.owner_id;
        tokio::spawn(async move {
            use std::time::Duration;
            loop {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let now = chrono::Utc::now();
                let (due, overdue) = handlers::money::reconciliation_cutoffs(now);
                let rows = db_bg.reconciliation_rows(&due, &overdue);
                for r in &rows {
                    if r.status == "executing"
                        && db_bg.mark_needs_reconciliation(&r.row_type, r.id, &due)
                    {
                        db_bg.log_action(
                            0,
                            "system",
                            "transfer_reconciliation_due",
                            &r.row_type,
                            r.id,
                            Some(&format!(
                                "spend_id={}",
                                r.spend_id.as_deref().unwrap_or("-")
                            )),
                        );
                    }
                    if r.overdue {
                        alert_once(
                            &db_bg,
                            &bm_bg,
                            owner_id,
                            "transfer_reconciliation_overdue",
                            &r.row_type,
                            &r.id.to_string(),
                            format!(
                                "BLOCKING: {} #{} ({} {}) is past the 24h owner-decision deadline. Resolve it in Finance.",
                                r.row_type, r.id, r.amount, r.asset
                            ),
                        )
                        .await;
                    }
                }
                if handlers::money::should_run_digest(now) {
                    let rows = handlers::money::all_reconciliation_rows(&db_bg, &due, &overdue);
                    let hash = handlers::money::digest_row_ids_hash(&rows);
                    let text = handlers::money::format_digest(&rows, now);
                    let meta = serde_json::json!({
                        "row_count": rows.len(), "hash": hash,
                        "overdue": rows.iter().filter(|r| r.overdue).count(),
                    })
                    .to_string();
                    db_bg.log_action(
                        0,
                        "system",
                        "transfer_reconciliation_digest",
                        "reconciliation",
                        "digest",
                        Some(&meta),
                    );
                    dm_owner(&bm_bg, owner_id, text).await;
                }
            }
        });
    }
    let db_cmd = db.clone();
    let cfg_cmd = cfg.clone();
    let crypto_cmd = crypto.clone();
    let ai_cmd = ai_service.clone();
    let db_plugin = db.clone();
    let cfg_plugin = cfg.clone();
    let db_photo = db.clone();
    let cfg_photo = cfg.clone();
    let db_crypto = db.clone();
    let cfg_crypto = cfg.clone();
    let db_ai_confirm = db.clone();
    let cfg_ai_confirm = cfg.clone();
    let db_ai_edit = db.clone();
    let cfg_ai_edit = cfg.clone();
    let db_cb = db.clone();
    let cfg_cb = cfg.clone();
    let db_idle = db.clone();

    // Plugin commands registered at startup (see handlers::registry). The
    // binary registers only the MCP approval commands (/approvals, /approve,
    // /deny); embedded hosts and forks can add theirs. Registration must
    // happen before the registry is frozen into an Arc, so the web state the
    // approval executors share with the MCP endpoint is built first.
    let base = cfg.web_base_path();
    let web_state = web::WebState {
        db: db.clone(),
        cfg: cfg.clone(),
        bot: bot.clone(),
        bots: bot_manager.clone(),
        identity,
        login_attempts: std::sync::Arc::new(
            std::sync::Mutex::new(std::collections::HashMap::new()),
        ),
        ai_test_inflight: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        base_path: base.clone(),
    };
    let mut registry = CommandRegistry::new();
    mcp::register_commands(&mut registry, web_state.clone());
    let registry = Arc::new(registry);
    let registry_for_commands = registry.clone();

    // Shared dependencies for live scoped verticals. The receiving bot's
    // context is injected per dispatcher by dptree, never captured here.
    let scoped_dispatch = handlers::capabilities::ScopedDispatch {
        db: db_cmd.clone(),
        cfg: cfg_cmd.clone(),
        bot_manager: bot_manager.clone(),
        crypto: crypto_cmd.clone(),
        ai: ai_cmd.clone(),
        registry: registry.clone(),
    };

    let handler = dptree::entry()
        .branch(
            Update::filter_message()
                // Multibot capability gate (server). The receiving bot's
                // `bot_type` selects its inbound handler. Shop and the live
                // scoped verticals fall through; compiled stubs, uncompiled
                // and unknown types get an explicit, logged notice. Never a
                // silent drop.
                .branch(
                    dptree::filter(|cap: Arc<handlers::capabilities::BotCapability>| {
                        cap.refuses_inbound()
                    })
                    .endpoint(
                        |bot: Bot,
                         msg: Message,
                         cap: Arc<handlers::capabilities::BotCapability>| {
                            async move {
                                handlers::capabilities::refuse_message(
                                    &bot,
                                    msg.chat.id,
                                    &cap,
                                    "message",
                                )
                                .await
                            }
                        },
                    ),
                )
                // End-user appeal reason collection (multibot foundation
                // Step 4.4, server). This branch runs BEFORE the inbound
                // moderation gate so the buyer's short reason is not itself
                // held by a blocked term. Reachable only from the single
                // "Request review" callback, which stores the decision's
                // ORIGINAL opaque reference in the dialogue — never the held
                // text. No state fields are bound with dptree::case! (see
                // AGENT.md startup-panic decision log).
                .branch(
                    dptree::entry()
                        .enter_dialogue::<Message, ErasedStorage<State>, State>()
                        .filter(|state: State| {
                            matches!(state, State::AwaitingAppealReason { .. })
                        })
                        .endpoint(
                            move |bot: Bot,
                                  msg: Message,
                                  dialogue: Dialogue<State, ErasedStorage<State>>,
                                  bot_ctx: Arc<bots::BotContext>| async move {
                                handlers::messages::awaiting_appeal_reason_handler(
                                    bot, msg, dialogue, bot_ctx,
                                )
                                .await
                            },
                        ),
                )
                // Cross-cutting inbound moderation gate (multibot foundation
                // Step 4.2). Runs for every message BEFORE dialogue entry or
                // command/text routing, so a hidden/review/degraded message
                // never reaches a handler. `evaluate_inbound_detailed` reads
                // only the effective policy, appends one opaque audit row,
                // and fails closed to a hold when enforcement is degraded.
                // Pass/warn fall through unchanged (warn is recorded, not
                // surfaced here — the warning UX stays deferred). For a
                // blocking verdict the buyer gets ONE content-free notice with
                // a "Request review" button carrying the SAME opaque reference
                // from the audit row.
                .branch(
                    dptree::filter_map(
                        |msg: Message, bot_ctx: Arc<bots::BotContext>| {
                            let text = msg
                                .text()
                                .or_else(|| msg.caption())
                                .unwrap_or_default()
                                .to_string();
                            let sender = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
                            let seller_id = handlers::moderation::inbound_seller_scope(
                                &bot_ctx.db,
                                bot_ctx.id,
                                sender,
                            );
                            let evaluation = handlers::moderation::evaluate_inbound_detailed(
                                &bot_ctx.db,
                                seller_id,
                                Some(bot_ctx.id),
                                &text,
                            );
                            evaluation
                                .mode
                                .blocks_inbound_routing()
                                .then_some((seller_id, evaluation))
                        },
                    )
                    .endpoint(
                        move |bot: Bot,
                              msg: Message,
                              bot_ctx: Arc<bots::BotContext>,
                              gated: (i64, handlers::moderation::ModerationEvaluation)| async move {
                            let (seller_id, evaluation) = gated;
                            let offer = handlers::moderation::AppealOffer {
                                seller_id,
                                bot_id: bot_ctx.id,
                                message_ref: evaluation.message_ref,
                                decision_id: evaluation.decision_id,
                            };
                            let _ = handlers::moderation::send_appeal_offer(
                                &bot,
                                &bot_ctx.db,
                                msg.chat.id,
                                &offer,
                            )
                            .await;
                            Ok(()) as HandlerResult
                        },
                    ),
                )
                .enter_dialogue::<Message, ErasedStorage<State>, State>()
                // Live scoped verticals use the same shipped handlers, but
                // only their own command family. Everything else they receive
                // ends at the scoped branch and never reaches shop-only code.
                .branch(
                    dptree::filter(|cap: Arc<handlers::capabilities::BotCapability>| {
                        cap.is_scoped()
                    })
                    .branch(
                        dptree::entry()
                            .filter_command::<handlers::commands::Command>()
                            .filter(
                                |cmd: handlers::commands::Command,
                                 cap: Arc<handlers::capabilities::BotCapability>| {
                                    cap.allows_command(&cmd)
                                },
                            )
                            .endpoint(
                                move |bot: Bot,
                                      msg: Message,
                                      dialogue: Dialogue<State, ErasedStorage<State>>,
                                      cmd: handlers::commands::Command,
                                      cap: Arc<handlers::capabilities::BotCapability>,
                                      bot_ctx: Arc<bots::BotContext>| {
                                    let deps = scoped_dispatch.clone();
                                    async move {
                                        handlers::capabilities::dispatch_scoped(
                                            bot, msg, cmd, dialogue, &cap, &deps, bot_ctx,
                                        )
                                        .await
                                    }
                                },
                            ),
                    )
                    .endpoint(
                        |bot: Bot,
                         msg: Message,
                         cap: Arc<handlers::capabilities::BotCapability>,
                         bot_ctx: Arc<bots::BotContext>| async move {
                            handlers::capabilities::handle_scoped_plain(&bot, &msg, &cap, &bot_ctx)
                                .await
                        },
                    ),
                )
                .branch(
                    dptree::case![State::Idle]
                        .branch(
                            dptree::entry()
                                .filter_command::<handlers::commands::Command>()
                                .endpoint(
                                    move |bot: Bot,
                                          msg: Message,
                                          dialogue: Dialogue<State, ErasedStorage<State>>,
                                          cmd: handlers::commands::Command,
                                          bot_ctx: Arc<bots::BotContext>,
                                          bot_manager: Arc<bots::BotManager>,
                                          owner_id: i64| {
                                        let db = db_cmd.clone();
                                        let cfg = cfg_cmd.clone();
                                        let crypto = crypto_cmd.clone();
                                        let ai = ai_cmd.clone();
                                        let reg = registry_for_commands.clone();
                                        async move {
                                            handlers::commands::cmd_handler(
                                                bot,
                                                msg,
                                                dialogue,
                                                db,
                                                cfg,
                                                bot_ctx,
                                                bot_manager,
                                                owner_id,
                                                crypto,
                                                ai,
                                                reg,
                                                cmd,
                                            )
                                            .await
                                        }
                                    },
                                ),
                        )
                        // Registered plugin commands: matched only when the
                        // text is `/name ...` naming a registered command
                        // (falls through otherwise), then role-gated like the
                        // built-ins before the handler runs.
                        .branch(
                            dptree::filter(|cap: Arc<handlers::capabilities::BotCapability>,
                                           reg: Arc<CommandRegistry>,
                                           msg: Message| {
                                cap.allows_plugin_commands()
                                    && plugin_command_name(&msg, &reg).is_some()
                            })
                            .endpoint(
                                move |bot: Bot,
                                      msg: Message,
                                      reg: Arc<CommandRegistry>,
                                      owner_id: i64| {
                                    let db = db_plugin.clone();
                                    let cfg = cfg_plugin.clone();
                                    async move {
                                        plugin_command_handler(bot, msg, db, cfg, reg, owner_id)
                                            .await
                                    }
                                },
                            ),
                        )
                        // Idle plain-text fall-through (B2): fires ONLY here —
                        // commands and plugin commands match the branches above,
                        // and every other dialogue state (AwaitingPhoto/
                        // AiPostConfirm/AiPostEdit/CryptoInvoice) is handled by
                        // its own branch below — so buyers mid-dialogue and
                        // non-text updates never reach it.
                        .branch(dptree::endpoint(
                            move |bot: Bot, msg: Message, owner_id: i64| {
                                let db = db_idle.clone();
                                async move { non_command_handler(bot, msg, db, owner_id).await }
                            },
                        )),
                )
                .branch(dptree::case![State::AwaitingPhoto].endpoint(
                    move |bot: Bot,
                          msg: Message,
                          dialogue: Dialogue<State, ErasedStorage<State>>,
                          bot_ctx: Arc<bots::BotContext>,
                          owner_id: i64| {
                        let db = db_photo.clone();
                        let cfg = cfg_photo.clone();
                        async move {
                            handlers::messages::awaiting_photo_handler(
                                bot, msg, dialogue, db, cfg, bot_ctx, owner_id,
                            )
                            .await
                        }
                    },
                ))
                .branch(
                    dptree::case![State::AwaitingAiPostConfirm { draft_id }].endpoint(
                        move |bot: Bot,
                              msg: Message,
                              dialogue: Dialogue<State, ErasedStorage<State>>,
                              draft_id: i64,
                              bot_ctx: Arc<bots::BotContext>| {
                            let db = db_ai_confirm.clone();
                            let cfg = cfg_ai_confirm.clone();
                            async move {
                                handlers::messages::awaiting_ai_post_confirm_handler(
                                    bot, msg, dialogue, db, cfg, bot_ctx, draft_id,
                                )
                                .await
                            }
                        },
                    ),
                )
                .branch(
                    dptree::case![State::AwaitingAiPostEdit { draft_id }].endpoint(
                        move |bot: Bot,
                              msg: Message,
                              dialogue: Dialogue<State, ErasedStorage<State>>,
                              draft_id: i64,
                              bot_ctx: Arc<bots::BotContext>| {
                            let db = db_ai_edit.clone();
                            let cfg = cfg_ai_edit.clone();
                            async move {
                                handlers::messages::awaiting_ai_post_edit_handler(
                                    bot, msg, dialogue, db, cfg, bot_ctx, draft_id,
                                )
                                .await
                            }
                        },
                    ),
                )
                // Catch-all for the remaining dialogue state (CryptoInvoice):
                // dptree's case! cannot bind struct-variant fields, and Idle/
                // AwaitingPhoto are handled above, so whatever falls through
                // here is the invoice wizard. The handler reads the state
                // itself from the dialogue.
                .branch(dptree::endpoint(
                    move |bot: Bot,
                          msg: Message,
                          dialogue: Dialogue<State, ErasedStorage<State>>,
                          bot_ctx: Arc<bots::BotContext>| {
                        let db = db_crypto.clone();
                        let cfg = cfg_crypto.clone();
                        async move {
                            handlers::messages::crypto_invoice_handler(
                                bot, msg, dialogue, db, cfg, bot_ctx,
                            )
                            .await
                        }
                    },
                )),
        )
        .branch(
            Update::filter_callback_query()
                // Same gate as the message path: answer the callback with an
                // explicit notice so no inline press is silently dropped.
                .branch(
                    dptree::filter(|cap: Arc<handlers::capabilities::BotCapability>| {
                        !cap.is_legacy_shop()
                    })
                    .endpoint(
                        |bot: Bot,
                         q: CallbackQuery,
                         cap: Arc<handlers::capabilities::BotCapability>| {
                            async move {
                                handlers::capabilities::refuse_callback(&bot, &q, &cap, "callback")
                                    .await
                            }
                        },
                    ),
                )
                .enter_dialogue::<CallbackQuery, ErasedStorage<State>, State>()
                .branch(dptree::endpoint(
                    move |bot: Bot,
                          q: CallbackQuery,
                          dialogue: Dialogue<State, ErasedStorage<State>>,
                          bot_ctx: Arc<bots::BotContext>,
                          owner_id: i64| {
                        let db = db_cb.clone();
                        let cfg = cfg_cb.clone();
                        async move {
                            handlers::callbacks::callback_handler(
                                bot, q, dialogue, db, cfg, bot_ctx, owner_id,
                            )
                            .await
                        }
                    },
                )),
        );

    // Inject dependencies. One Dispatcher per bot is built lazily below
    // (see bot_task) so each closes over its own bot handle.

    // Web UI state.
    let web_router = web::router(web_state.clone());
    let api_router = api::router(web_state.clone());
    let web_router = web::mount(
        &base,
        web_router
            .merge(api_router)
            .merge(mcp::router(web_state))
            // 30s request ceiling for the whole control plane (server). The bot
            // long-poll never passes through axum and the cryptopay webhook
            // is two DB writes, so nothing legitimate needs longer (see
            // web::REQUEST_TIMEOUT).
            .layer(axum::middleware::from_fn(web::request_timeout))
            // Security headers on every response (security): outside the timeout
            // layer so aborted requests still carry them; metrics below stays
            // outermost so those responses are still counted.
            .layer(axum::middleware::from_fn(security_headers))
            // HTTP metrics outermost, so requests aborted by the timeout
            // layer are still counted (status_class="5xx"); labels come from
            // the matched route template only (see src/metrics.rs).
            .layer(axum::middleware::from_fn(metrics::track_http)),
    );
    let web_port: u16 = cfg.web_port();
    let web_bind = cfg.web_bind();
    let listener = match tokio::net::TcpListener::bind(format!("{web_bind}:{web_port}")).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(
                "failed to bind {web_bind}:{web_port}: {e}; exiting so systemd can restart"
            );
            // Non-zero: a bind failure must trigger Restart=on-failure, not
            // leave a half-dead process that answers no requests.
            std::process::exit(1);
        }
    };
    tracing::info!(
        "Web UI listening on http://{}:{} (localhost: http://127.0.0.1:{})",
        web_bind,
        web_port,
        web_port
    );
    if cfg.open_browser() {
        open_in_browser(&format!("http://127.0.0.1:{web_port}{base}"));
    }

    // Setup mode: without a bot token the poller cannot start (Telegram
    // rejects getMe and teloxide aborts the task), so run the web UI only —
    // configure there, then restart to bring the bot online.
    let bot_task = if app_cfg.is_configured() && !bot_contexts.is_empty() {
        let mut tasks = Vec::new();
        let mut first = true;
        for ctx in bot_contexts.clone() {
            // One capability selection per dispatcher: this dispatcher only
            // ever serves `ctx`, so the gate sees the receiving bot's type.
            let capability = Arc::new(bot_manager.capability_for_bot(ctx.id));
            let mut builder = Dispatcher::builder(ctx.bot.clone(), handler.clone()).dependencies(
                // `bot_manager` is injected (not captured per
                // endpoint) so per-seller moderation such as
                // /expirecheck acts through the bot a seller is bound
                // to, not the bot that received the command.
                dptree::deps![
                    storage.clone(),
                    ctx.clone(),
                    ctx.owner_id,
                    registry.clone(),
                    bot_manager.clone(),
                    capability
                ],
            );
            if first {
                builder = builder.enable_ctrlc_handler();
                first = false;
            }
            let mut dispatcher = builder.build();
            // Explicit long-poll listener instead of `dispatch()` (whose
            // internal `polling_default` hardcodes 10s): a 30s poll costs one
            // round trip per cycle on this network. A poll wedged past
            // bots::BOT_HTTP_TOTAL_TIMEOUT is cut off by the client and
            // resurfaces as a Network error, which teloxide's polling backoff
            // retries indefinitely — the loop recovers on its own.
            tasks.push((
                ctx.key.clone(),
                tokio::spawn(async move {
                    let listener = Polling::builder(ctx.bot.clone())
                        .timeout(bots::LONG_POLL_TIMEOUT)
                        .delete_webhook()
                        .await
                        .build();
                    dispatcher
                        .dispatch_with_listener(
                            listener,
                            LoggingErrorHandler::with_custom_text(
                                "An error from the update listener",
                            ),
                        )
                        .await;
                }),
            ));
        }
        // Startup delivery self-check (one DM per started bot, once per boot
        // — not per reconnect): get_me() for the username, then DM the bot's
        // owner. A failed DM is logged loudly (that is the "bot can't DM me"
        // diagnosis at boot) and never aborts startup; fire-and-forget — the
        // dispatchers below start regardless of the outcome.
        for ctx in bot_contexts.clone() {
            tokio::spawn(async move {
                let chat = teloxide::types::ChatId(ctx.owner_id);
                match ctx.bot.get_me().await {
                    Ok(me) => {
                        let username = me.username.as_deref().unwrap_or("<no username>");
                        let notice = format!(
                            "✅ Bot @{username} is connected and polling — DM delivery to you works."
                        );
                        match ctx.bot.send_message(chat, notice).await {
                            Ok(_) => metrics::bot_sent(),
                            Err(e) => tracing::error!(
                                bot = %ctx.key,
                                "startup self-check: bot {} could not DM its owner ({}): {}",
                                ctx.key,
                                chat.0,
                                e
                            ),
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            bot = %ctx.key,
                            "startup self-check: bot {} get_me failed — the token is likely invalid: {}",
                            ctx.key,
                            e
                        );
                    }
                }
            });
        }
        Some(tokio::spawn(async move {
            // A dispatcher task must never die silently: that is the deaf-bot
            // state (process alive, web serving, no poller) that restart
            // supervision cannot see. Panics here include teloxide's own
            // startup `get_me` failure (`dispatch_with_listener` expects the
            // initial call) — log loudly and keep the other bots serving.
            for (key, t) in tasks {
                match t.await {
                    Ok(()) => tracing::info!(bot = %key, "bot dispatcher finished"),
                    Err(e) => tracing::error!(bot = %key, "bot dispatcher task died: {e}"),
                }
            }
        }))
    } else {
        tracing::info!("bot polling disabled until configured (setup mode)");
        None
    };
    // ConnectInfo: the web claim handlers (v1.3.x) read the peer address via
    // ConnectInfo<SocketAddr> so codeless claiming works only from loopback —
    // without the make-service wrapper axum cannot see peer addresses.
    let web_task = tokio::spawn(async move {
        match axum::serve(
            listener,
            web_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            Ok(()) => tracing::info!("web server stopped"),
            Err(e) => {
                // Previously `.unwrap()` aborted the task with a panic that
                // nothing surfaced as a failed process. Fail loudly instead.
                tracing::error!("web server failed: {e}; exiting non-zero");
                std::process::exit(1);
            }
        }
    });

    // Shutdown must be reachable from `systemctl stop`, not just an interactive
    // Ctrl-C. SIGTERM takes the SAME graceful path below (WAL checkpoint +
    // teardown); without this arm systemd waits for its kill timeout and
    // SIGKILLs the process, leaving an uncheckpointed database.
    #[cfg(unix)]
    let shutdown = async {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { tracing::info!("shutdown signal received"); }
            _ = async {
                match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                    Ok(mut sigterm) => {
                        sigterm.recv().await;
                        tracing::info!("SIGTERM received");
                    }
                    Err(e) => {
                        tracing::error!("failed to install SIGTERM handler: {e}");
                        // Never let a missing handler turn into a busy loop.
                        std::future::pending::<()>().await;
                    }
                }
            } => {}
        }
    };
    #[cfg(not(unix))]
    let shutdown = async {
        tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received");
    };

    tokio::select! {
        _ = shutdown => {}
        _ = async {
            match bot_task {
                Some(t) => { let _ = t.await; }
                None => std::future::pending::<()>().await,
            }
        } => { tracing::info!("bot task ended"); }
        _ = web_task => { tracing::info!("web task ended"); }
    }
    // Flush the WAL into the main database file before exiting
    // (PRAGMA wal_checkpoint(TRUNCATE)) so backups and file copies see a
    // complete database instead of a main file plus a trailing -wal log.
    // `db` is still owned here — only clones moved into tasks/states — and
    // the background loops are parked in sleeps by the time a signal lands,
    // so the checkpoint normally runs uncontended. Best-effort: failures are
    // logged inside Db::checkpoint and never fatal.
    db.checkpoint();
    // Cancel remaining tasks.
    // (tasks are dropped on select exit; nothing further to do)
}

/// Idle plain-text fall-through (B2). The dptree placement (inside
/// `case![State::Idle]`, after the command and plugin-command branches) means
/// this fires only for Idle updates that are neither — photos and other
/// non-text updates, unknown `/commands` and every buyer DM stay silent,
/// unchanged. The one exception: the bot's owner, who gets a single
/// discoverability tip for /post and /schedule (sellers/admins keep the
/// documented help surface; noise stays off their DMs too).
async fn non_command_handler(bot: Bot, msg: Message, db: Arc<Db>, owner_id: i64) -> HandlerResult {
    let Some(text) = msg.text() else {
        return Ok(());
    };
    // Unknown commands (text starting with '/') fall through here too; they
    // keep the silent treatment — replying to a mistyped command with the tip
    // would be noise, not help.
    if text.trim_start().starts_with('/') {
        return Ok(());
    }
    let Some(from) = msg.from.as_ref() else {
        return Ok(());
    };
    let user_id = from.id.0 as i64;
    // Strict Owner only: role_of short-circuits on the owner id, so this
    // costs buyers nothing but two small lookups (is_admin, get_seller).
    if roles::role_of(&db, user_id, owner_id) != roles::Role::Owner {
        return Ok(());
    }
    bot.send_message(
        msg.chat.id,
        "Tip: /post <text> publishes to the channel — /schedule <YYYY-MM-DD HH:MM> <text> schedules it (UTC).",
    )
    .await?;
    Ok(())
}

/// The registered command named by `msg`, if the text starts with `/` and the
/// first word (minus the leading `/`) names a registered command.
fn plugin_command_name(msg: &Message, reg: &CommandRegistry) -> Option<String> {
    let text = msg.text()?;
    let first = text.split_whitespace().next()?;
    let name = first.strip_prefix('/')?;
    if name.is_empty() {
        return None;
    }
    reg.get(name).map(|_| name.to_string())
}

/// Dispatch a registered plugin command: parse the args (the text after the
/// command word), resolve the caller's role, and silently ignore callers below
/// the command's `min_role` — mirroring the built-in commands' gate.
async fn plugin_command_handler(
    bot: Bot,
    msg: Message,
    db: Arc<Db>,
    cfg: Config,
    reg: Arc<CommandRegistry>,
    owner_id: i64,
) -> HandlerResult {
    let Some(name) = plugin_command_name(&msg, &reg) else {
        return Ok(());
    };
    let Some(spec) = reg.get(&name) else {
        return Ok(());
    };
    let args = msg
        .text()
        .and_then(|t| t.split_once(char::is_whitespace))
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_default();
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    let role = roles::role_of(&db, user_id, owner_id);
    if role < spec.min_role {
        return Ok(());
    }
    let ctx = CommandContext {
        bot,
        msg,
        db,
        cfg,
        args,
        role,
    };
    (spec.handler)(ctx).await?;
    Ok(())
}

/// Open the dashboard in the system browser (desktop app UX). Fire-and-forget:
/// a failure to open a browser must never stop the server. Each platform
/// launcher is a fixed program invoked with the URL as a single argument —
/// no shell, no string-built command lines.
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("explorer").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    if let Err(e) = result {
        tracing::warn!("could not open browser: {e}");
    }
}
