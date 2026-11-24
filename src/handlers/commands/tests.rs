use super::*;

#[test]
fn plan_add_parses() {
    assert_eq!(
        parse_plan_add("Gold | 9.9 | USDT | 30"),
        Ok(("Gold", "9.9", "USDT", 30))
    );
    assert_eq!(parse_plan_add("p|1|USDT|1"), Ok(("p", "1", "USDT", 1)));
    assert_eq!(
        parse_plan_add("p | 1 | USDT | 3650"),
        Ok(("p", "1", "USDT", MAX_PLAN_DAYS))
    );
}

#[test]
fn plan_add_rejects() {
    // bad numbers and boundary days: 0 and 3651 are outside 1..=3650
    for bad in ["0", "3651", "abc", "", "-1"] {
        assert_eq!(
            parse_plan_add(&format!("p | 1 | USDT | {}", bad)),
            Err(PlanAddError::Days(bad.to_string()))
        );
    }
    assert_eq!(parse_plan_add("p | 1 | USDT"), Err(PlanAddError::Usage));
    assert_eq!(parse_plan_add(""), Err(PlanAddError::Usage));
}

#[test]
fn product_add_valid() {
    assert_eq!(
        parse_product_add("VPN | 5 | USDT | digital_link | https://ex.co | 10"),
        Ok((
            "VPN",
            "5",
            "USDT",
            "digital_link",
            Some("https://ex.co"),
            Some(10)
        ))
    );
    // '-' delivery and '-' / blank / omitted stock all mean None
    assert_eq!(
        parse_product_add("Box | 3 | TON | physical | - | -"),
        Ok(("Box", "3", "TON", "physical", None, None))
    );
    assert_eq!(
        parse_product_add("Box | 3 | TON | physical | - |"),
        Ok(("Box", "3", "TON", "physical", None, None))
    );
}

#[test]
fn product_add_invalid() {
    // missing delivery field / empty name
    assert_eq!(
        parse_product_add("VPN | 5 | USDT | digital_link"),
        Err(ProductAddError::Usage)
    );
    assert_eq!(
        parse_product_add(" | 5 | USDT | physical | d"),
        Err(ProductAddError::Usage)
    );
    assert_eq!(
        parse_product_add("n | 1 | USDT | ebook | d"),
        Err(ProductAddError::Kind("ebook".into()))
    );
    assert_eq!(
        parse_product_add("n | 1 | USDT | digital_file | -"),
        Err(ProductAddError::Delivery)
    );
    assert_eq!(
        parse_product_add("n | 1 | USDT | physical | d | x"),
        Err(ProductAddError::Stock("x".into()))
    );
}

#[test]
fn order_set_parses() {
    assert_eq!(parse_order_set("A1 shipped"), Some(("A1", "shipped", None)));
    assert_eq!(
        parse_order_set("  A1   shipped  RR123 left depot "),
        Some(("A1", "shipped", Some("RR123 left depot".to_string())))
    );
    assert_eq!(parse_order_set("A1"), None);
    assert_eq!(parse_order_set(""), None);
}

#[test]
fn gen_codes_parses() {
    assert_eq!(
        parse_gen_codes("basic | 5 | 30"),
        Some(("basic", 5, Some(30)))
    );
    assert_eq!(parse_gen_codes("basic"), Some(("basic", 1, None)));
    assert_eq!(parse_gen_codes("basic | x | y"), Some(("basic", 1, None)));
    assert_eq!(parse_gen_codes(" | 5 | 30"), None);
}

#[test]
fn split_channel_arg_variants() {
    assert_eq!(
        split_channel_arg("@chan rest here"),
        (Some("@chan".into()), "rest here")
    );
    assert_eq!(
        split_channel_arg("-100123 rest"),
        (Some("-100123".into()), "rest")
    );
    assert_eq!(split_channel_arg("just text"), (None, "just text"));
    assert_eq!(split_channel_arg(""), (None, ""));
}

#[test]
fn schedule_datetime_formats() {
    assert_eq!(
        parse_schedule_datetime("2026-01-02T03:04:05Z"),
        Some("2026-01-02T03:04:05+00:00".into())
    );
    assert_eq!(
        parse_schedule_datetime("2026-01-02 03:04"),
        Some("2026-01-02T03:04:00+00:00".into())
    );
    assert_eq!(
        parse_schedule_datetime("2026-01-02"),
        Some("2026-01-02T00:00:00+00:00".into())
    );
    assert_eq!(parse_schedule_datetime("tomorrow"), None);
}

// ---- /schedule parse+create checked (A7): refuse without storing ----

#[test]
fn schedule_checked_refuses_unparseable_without_storing() {
    // A total parse failure returns the fix-naming error AND stores no row:
    // the raw string used to land in scheduled_posts, where the due queue
    // could never fire it correctly (lexicographic compare) — the A6 poison.
    let (_d, db) = chan_db();
    for bad in ["tomorrow", "next friday", "2026/01/02 03:04", ""] {
        let err = create_schedule_checked(&db, 7, "@mine", "hello", bad)
            .expect_err("unparseable dates must be refused");
        assert!(err.contains(bad), "error names the raw input: {err}");
        assert!(
            err.contains("YYYY-MM-DD HH:MM"),
            "error names the format: {err}"
        );
        assert!(
            db.list_scheduled_posts(Some(7)).is_empty(),
            "a refused schedule must not store a row"
        );
    }
}

#[test]
fn schedule_checked_stores_lenient_parses() {
    // The leniency is unchanged: RFC3339, 'T'-separated, 'YYYY-MM-DD HH:MM'
    // and date-only (UTC midnight) all store one row with a normalized
    // RFC3339 scheduled_at.
    let (_d, db) = chan_db();
    for (input, want) in [
        ("2026-01-02T03:04:05Z", "2026-01-02T03:04:05+00:00"),
        ("2026-01-02 03:04", "2026-01-02T03:04:00+00:00"),
        ("2026-01-02", "2026-01-02T00:00:00+00:00"),
    ] {
        let (id, stored) = create_schedule_checked(&db, 7, "@mine", "hello", input)
            .unwrap_or_else(|e| panic!("'{input}' must parse: {e}"));
        assert!(id > 0);
        assert_eq!(stored, want);
    }
    let rows = db.list_scheduled_posts(Some(7));
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|r| r.6 == "pending"));
}

// ---- /scheduled + /cancelpost parsing and listing helpers ----

#[test]
fn parse_scheduled_and_cancelpost() {
    let c = Command::parse("/scheduled", "bot").unwrap();
    assert!(matches!(c, Command::Scheduled));
    let c = Command::parse("/cancelpost 12", "bot").unwrap();
    assert!(matches!(c, Command::CancelPost(ref s) if s == "12"));
}

#[test]
fn text_preview_truncates_at_char_boundary() {
    assert_eq!(text_preview("short", 40), "short");
    let long = "x".repeat(41);
    assert_eq!(text_preview(&long, 40), format!("{}…", "x".repeat(40)));
    // Exactly 40 chars: no ellipsis.
    let edge = "y".repeat(40);
    assert_eq!(text_preview(&edge, 40), edge);
    // Multi-byte chars count as chars, never split mid-glyph.
    let uni = "é".repeat(41);
    assert_eq!(text_preview(&uni, 40), format!("{}…", "é".repeat(40)));
}

// ---- resolve_target_channel: seller tenancy on channel-scoped commands ----

fn chan_cfg() -> SharedConfig {
    SharedConfig {
        owner_id: 1,
        db_path: String::new(),
        crypto_token: None,
        crypto_api_base: None,
        ai_enabled: false,
        mfa_recovery_codes_enabled: true,
        ai_provider: "openai".to_string(),
        ai_api_style: "openai".to_string(),
        ai_base_url: "https://api.openai.com/v1".to_string(),
        ai_api_key: None,
        ai_model: "gpt-4o-mini".to_string(),
        ai_daily_token_budget: None,
        mcp_owner_token: None,
        telegram_api_id: None,
        telegram_api_hash: None,
        bots: Vec::new(),
        bots_legacy: true,
    }
}

fn chan_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("chan.db").to_str().unwrap()).unwrap();
    (dir, db)
}

#[test]
fn target_channel_seller_locked_to_own() {
    // A seller may only target their own seller_channel: an explicit
    // @channel matching their own is accepted, any other channel is
    // rejected, and the no-channel case fails closed with the
    // /setchannel hint — with or without an explicit argument.
    let (_d, db) = chan_db();
    let cfg = chan_cfg();
    db.set_seller_channel(7, "@mine");
    assert_eq!(
        resolve_target_channel(&db, &cfg, 7, false, None),
        Ok("@mine".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 7, false, Some("@mine".into())),
        Ok("@mine".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 7, false, Some("@other".into())),
        Err("You can only target your own channel. Set it with /setchannel.".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 8, false, None),
        Err("No channel set. Use /setchannel @channel first.".to_string())
    );
    assert!(
        resolve_target_channel(&db, &cfg, 8, false, Some("@other".into())).is_err(),
        "an unconfigured seller may not pass an explicit channel"
    );
}

#[test]
fn target_channel_admin_keeps_explicit_wins() {
    // Admin/Owner: the explicit argument wins verbatim (any channel the
    // bot is a member of), otherwise the channel_of fallback chain —
    // with the caller's own seller channel still taking precedence over
    // the global default. Sellers never inherit the global default.
    let (_d, db) = chan_db();
    let cfg = chan_cfg();
    db.set_config("channel", "@global");
    assert_eq!(
        resolve_target_channel(&db, &cfg, 1, true, Some("@any".into())),
        Ok("@any".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 1, true, None),
        Ok("@global".to_string())
    );
    // An admin who also owns a seller channel: explicit still wins, and
    // without an argument their own channel comes before the global.
    db.set_seller_channel(1, "@mine");
    assert_eq!(
        resolve_target_channel(&db, &cfg, 1, true, Some("@any".into())),
        Ok("@any".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 1, true, None),
        Ok("@mine".to_string())
    );
    assert_eq!(
        resolve_target_channel(&db, &cfg, 9, false, None),
        Err("No channel set. Use /setchannel @channel first.".to_string())
    );
}

// ---- /buy resolution over the (seller_id, name) plan namespace ----

fn buy_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::new(dir.path().join("buy.db").to_str().unwrap()).unwrap();
    (dir, db)
}

#[test]
fn buy_resolution_is_seller_scoped() {
    // Cross-seller plan isolation (2026-10-25 review HIGH): two sellers
    // may both define "pro" and each resolves only their own; a name
    // unique to another seller is invisible from the wrong namespace.
    let (_d, db) = buy_db();
    db.add_plan(1, "pro", "10", "USDT", 30, None);
    db.add_plan(2, "pro", "20", "USDT", 30, None);
    let (plans, products) = resolve_buy_candidates(&db, roles::Role::Seller, 1, "pro");
    assert!(products.is_empty());
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].1, "10");
    assert_eq!(plans[0].6, 1);
    let (plans2, _) = resolve_buy_candidates(&db, roles::Role::Seller, 2, "pro");
    assert_eq!(plans2[0].1, "20");
    db.add_plan(2, "solo", "5", "TON", 7, None);
    let (solo, _) = resolve_buy_candidates(&db, roles::Role::Seller, 1, "solo");
    assert!(solo.is_empty(), "seller 1 must not resolve seller 2's plan");
    // Same isolation for products (the /buy multiplex resolves both).
    db.add_product(2, "widget", "3", "TON", "digital_link", Some("l"), None)
        .unwrap();
    let (p, w) = resolve_buy_candidates(&db, roles::Role::Seller, 1, "widget");
    assert!(p.is_empty() && w.is_empty());
    let (_, w2) = resolve_buy_candidates(&db, roles::Role::Seller, 2, "widget");
    assert_eq!(w2.len(), 1);
}

#[test]
fn buy_resolution_global_namespace_admin_owner_only() {
    // The global (seller_id = 0) namespace — legacy rows and admin-created
    // catalogue entries — is resolvable only by Admin/Owner (B5).
    let (_d, db) = buy_db();
    db.add_plan(0, "legacy", "3", "USDT", 30, None);
    let (p_user, _) = resolve_buy_candidates(&db, roles::Role::User, 7, "legacy");
    assert!(
        p_user.is_empty(),
        "plain users cannot resolve the global namespace"
    );
    let (p_seller, _) = resolve_buy_candidates(&db, roles::Role::Seller, 1, "legacy");
    assert!(
        p_seller.is_empty(),
        "sellers cannot resolve the global namespace"
    );
    let (p_admin, _) = resolve_buy_candidates(&db, roles::Role::Admin, 1, "legacy");
    assert_eq!(p_admin.len(), 1);
    assert_eq!(p_admin[0].6, 0);
    let (p_owner, _) = resolve_buy_candidates(&db, roles::Role::Owner, 1, "legacy");
    assert_eq!(p_owner.len(), 1);
}

#[test]
fn buy_resolution_plain_user_shop_fallback_excludes_global() {
    // Public-mode /buy (README "Two modes"): a plain buyer falls back to
    // the cross-seller active shop view, minus the global namespace,
    // and the Owner may resolve any seller's entry by name.
    let (_d, db) = buy_db();
    db.add_plan(1, "vip", "9", "USDT", 30, None);
    db.add_plan(0, "gplan", "1", "USDT", 7, None);
    let (p, _) = resolve_buy_candidates(&db, roles::Role::User, 42, "vip");
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].6, 1);
    let (g, _) = resolve_buy_candidates(&db, roles::Role::User, 42, "gplan");
    assert!(
        g.is_empty(),
        "global namespace excluded from the shop fallback"
    );
    // Owner superuser ("owner can" in the review's test spec): any seller.
    let (o, _) = resolve_buy_candidates(&db, roles::Role::Owner, 999, "vip");
    assert_eq!(o.len(), 1);
    assert_eq!(o[0].6, 1);
}

#[test]
fn buy_resolution_ambiguous_name_still_reports_both() {
    // Seller-disambiguation syntax is deferred (v1.2 record): an
    // ambiguous name for a plain buyer returns multiple candidates so the
    // handler errors out instead of silently picking one.
    let (_d, db) = buy_db();
    db.add_plan(1, "dup", "1", "USDT", 7, None);
    db.add_plan(2, "dup", "2", "TON", 7, None);
    let (p, _) = resolve_buy_candidates(&db, roles::Role::User, 42, "dup");
    assert_eq!(p.len(), 2);
    // A seller with their own "dup" is never ambiguous: own namespace wins.
    db.add_plan(1, "dup", "3", "USDT", 7, None);
    let (p1, _) = resolve_buy_candidates(&db, roles::Role::Seller, 1, "dup");
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].1, "3");
}

#[test]
fn parse_referral() {
    let c = Command::parse("/referral", "bot").unwrap();
    assert!(matches!(c, Command::Referral));
}

#[test]
fn parse_redeemref() {
    let c = Command::parse("/redeemref ABC", "bot").unwrap();
    assert!(matches!(c, Command::RedeemRef(_)));
}

#[test]
fn parse_balance() {
    let c = Command::parse("/balance", "bot").unwrap();
    assert!(matches!(c, Command::Balance));
}

#[test]
fn parse_redeembonus() {
    let c = Command::parse("/redeembonus 100", "bot").unwrap();
    assert!(matches!(c, Command::RedeemBonus(_)));
}

#[test]
fn parse_customers() {
    let c = Command::parse("/customers", "bot").unwrap();
    assert!(matches!(c, Command::Customers));
}

#[test]
fn parse_tickets() {
    let c = Command::parse("/tickets", "bot").unwrap();
    assert!(matches!(c, Command::Tickets));
}

#[test]
fn parse_review() {
    let c = Command::parse("/review widget 5 great", "bot").unwrap();
    assert!(matches!(c, Command::Review(_)));
}

#[test]
fn parse_reorder() {
    let c = Command::parse("/reorder widget 7", "bot").unwrap();
    assert!(matches!(c, Command::Reorder(_)));
}

#[test]
fn parse_exportdata() {
    let c = Command::parse("/exportdata 42", "bot").unwrap();
    assert!(matches!(c, Command::ExportData(_)));
}

#[test]
fn parse_deletedata() {
    let c = Command::parse("/deletedata 42", "bot").unwrap();
    assert!(matches!(c, Command::DeleteData(_)));
}

#[test]
fn parse_support() {
    let c = Command::parse("/support help me", "bot").unwrap();
    assert!(matches!(c, Command::Support(_)));
}

#[test]
fn parse_reply() {
    let c = Command::parse("/reply 7 | thanks", "bot").unwrap();
    assert!(matches!(c, Command::Reply(_)));
}

#[test]
fn parse_closeticket() {
    let c = Command::parse("/closeticket 7", "bot").unwrap();
    assert!(matches!(c, Command::CloseTicket(_)));
}

// ---- /books alias: same parsed command, same handler path as /bookings ----

#[test]
fn books_alias_resolves_to_bookings() {
    let alias = Command::parse("/books", "bot").expect("/books must parse");
    let canonical = Command::parse("/bookings", "bot").expect("/bookings must parse");
    assert_eq!(alias, Command::Bookings);
    assert_eq!(canonical, Command::Bookings);
    assert_eq!(
        alias, canonical,
        "both spellings must take the exact same handler path"
    );
}

// ---- WEB_URL gate: dashboard setup never DMs a password ----

static WEB_URL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Save/restore WEB_URL even when a test panics. Only the tests in this module
/// take `WEB_URL_ENV_LOCK`; the value used below is plain http so
/// `auth::secure_cookies()` stays false for unrelated parallel tests.
struct WebUrlEnvGuard(Option<String>);

impl WebUrlEnvGuard {
    fn set(value: Option<&str>) -> Self {
        let previous = std::env::var("WEB_URL").ok();
        match value {
            Some(v) => unsafe { std::env::set_var("WEB_URL", v) },
            None => unsafe { std::env::remove_var("WEB_URL") },
        }
        Self(previous)
    }
}

impl Drop for WebUrlEnvGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(v) => unsafe { std::env::set_var("WEB_URL", v) },
            None => unsafe { std::env::remove_var("WEB_URL") },
        }
    }
}

fn cmd_test_db() -> (tempfile::TempDir, Arc<Db>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Db::new(dir.path().join("cmd.db").to_str().unwrap()).unwrap());
    (dir, db)
}

/// Drive the real `cmd_handler` against a wiremock Bot API and return the
/// `text` of every outgoing sendMessage. No Telegram transport is touched.
async fn run_cmd_with_mock_bot(cmd: Command, db: Arc<Db>) -> Vec<String> {
    use teloxide::dispatching::dialogue::{Dialogue, InMemStorage, Storage};
    use teloxide::types::ChatId;

    const USER: i64 = 42;
    const OWNER: i64 = 999;

    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 1,
                    "date": 1_700_000_000,
                    "chat": {"id": USER, "type": "private", "first_name": "T"},
                    "text": "sent"
                }
            })),
        )
        .mount(&server)
        .await;

    let bot = teloxide::Bot::new("42:placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let cfg = chan_cfg();
    let bot_ctx = Arc::new(crate::bots::BotContext {
        id: 0,
        key: "primary".to_string(),
        bot: bot.clone(),
        owner_id: OWNER,
        shared: Arc::new(cfg.clone()),
        db: db.clone(),
    });
    let bot_manager = Arc::new(crate::bots::BotManager {
        bots: vec![bot_ctx.clone()],
        primary: bot_ctx.clone(),
        db: db.clone(),
        identity: crate::identity::IdentityHandle::unconnected(),
    });
    let msg: teloxide::types::Message = serde_json::from_value(serde_json::json!({
        "message_id": 7,
        "date": 1_700_000_000,
        "chat": {"id": USER, "type": "private", "first_name": "T"},
        "from": {"id": USER, "is_bot": false, "first_name": "T", "username": "seller42"},
        "text": "/command"
    }))
    .expect("test Message must deserialize");
    let dialogue = Dialogue::new(InMemStorage::<State>::new().erase(), ChatId(USER));

    cmd_handler(
        bot,
        msg,
        dialogue,
        db,
        cfg,
        bot_ctx,
        bot_manager,
        OWNER,
        None,
        None,
        Arc::new(CommandRegistry::new()),
        cmd,
    )
    .await
    .expect("command handler must succeed");

    server
        .received_requests()
        .await
        .expect("mock Bot API requests")
        .iter()
        .map(|req| {
            let body: serde_json::Value =
                serde_json::from_slice(&req.body).expect("sendMessage JSON");
            body["text"].as_str().unwrap_or_default().to_string()
        })
        .collect()
}

/// `cmd_handler` is a very large async state machine; the production runtime
/// uses a 16 MiB worker stack (main.rs). Test threads default to 2 MiB, so run
/// handler-driving futures on a comparably sized stack.
fn on_large_stack<F, T>(future: F) -> T
where
    F: std::future::Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
        .name("cmd_test".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime")
                .block_on(future)
        })
        .expect("spawn test thread")
        .join()
        .expect("test thread must not panic")
}

#[test]
fn web_url_gate_is_pure_and_fail_closed() {
    assert_eq!(dashboard_setup_blocked(None), Some(WEB_URL_REQUIRED_MSG));
    assert_eq!(
        dashboard_setup_blocked(Some("")),
        Some(WEB_URL_REQUIRED_MSG)
    );
    assert_eq!(
        dashboard_setup_blocked(Some("   ")),
        Some(WEB_URL_REQUIRED_MSG)
    );
    // An explicitly configured base proceeds; the bot never invents one.
    assert_eq!(dashboard_setup_blocked(Some("http://127.0.0.1:8080")), None);
    let msg = dashboard_setup_blocked(None).expect("must refuse when WEB_URL unset");
    assert!(msg.contains("WEB_URL"), "{msg}");
    assert!(msg.contains("owner"), "{msg}");
    assert!(!msg.to_lowercase().contains("password"), "{msg}");
}

#[test]
fn no_plaintext_password_dm_when_web_url_unset() {
    let _lock = WEB_URL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = WebUrlEnvGuard::set(None);

    on_large_stack(async {
        for cmd in [Command::BecomeSeller, Command::Dashboard] {
            let (_dir, db) = cmd_test_db();
            db.upsert_seller(42, Some("seller42"), None);
            let texts = run_cmd_with_mock_bot(cmd.clone(), db.clone()).await;

            assert_eq!(texts.len(), 1, "only the refusal may be sent: {texts:?}");
            assert_eq!(texts[0], WEB_URL_REQUIRED_MSG, "{cmd:?}");
            for text in &texts {
                let lower = text.to_lowercase();
                assert!(!lower.contains("password"), "no password in DM: {text}");
                assert!(!lower.contains("secret"), "no secret in DM: {text}");
                assert!(
                    !text.contains("http"),
                    "no claim/URL material in DM: {text}"
                );
            }
            assert!(
                db.get_web_account_by_username("seller42").is_none(),
                "no account (and therefore no password) may be created when WEB_URL is unset: {cmd:?}"
            );
        }
    });
}

#[test]
fn claim_link_still_issued_when_web_url_set() {
    let _lock = WEB_URL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = WebUrlEnvGuard::set(Some("http://127.0.0.1:8080"));

    on_large_stack(async {
        let (_dir, db) = cmd_test_db();
        let texts = run_cmd_with_mock_bot(Command::BecomeSeller, db.clone()).await;
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(
            texts[0].contains("http://127.0.0.1:8080/setup/claim?token="),
            "one-time claim link must be issued: {}",
            texts[0]
        );
        assert!(
            !texts[0].to_lowercase().contains("password:"),
            "claim DM must not carry a password: {}",
            texts[0]
        );
        assert!(db.get_web_account_by_username("seller42").is_some());

        // The same account's /dashboard path also mints a fresh one-time link.
        db.upsert_seller(42, Some("seller42"), None);
        let dashboard_texts = run_cmd_with_mock_bot(Command::Dashboard, db).await;
        assert_eq!(dashboard_texts.len(), 1, "{dashboard_texts:?}");
        assert!(
            dashboard_texts[0].contains("http://127.0.0.1:8080/setup/claim?token="),
            "{}",
            dashboard_texts[0]
        );
        assert!(
            !dashboard_texts[0].to_lowercase().contains("password:"),
            "{}",
            dashboard_texts[0]
        );
    });
}
