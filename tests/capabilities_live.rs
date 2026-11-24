//! Inbound behavior of the live scoped capabilities. Each test drives the
//! real command handler (`dispatch_scoped` -> `cmd_handler`) against a mock
//! Bot API, so the reply comes from shipped code rather than capability copy.

use std::future::Future;
use std::sync::{Arc, Mutex};

use daybook::bots::{BotContext, BotManager};
use daybook::config::SharedConfig;
use daybook::db::Db;
use daybook::dialogue::State;
use daybook::handlers::capabilities::{
    BotCapability, ScopedDispatch, dispatch_scoped, handle_scoped_plain,
};
use daybook::handlers::commands::Command;
use daybook::handlers::registry::CommandRegistry;
use daybook::identity::IdentityHandle;
use teloxide::dispatching::dialogue::{Dialogue, InMemStorage, Storage};
use teloxide::prelude::*;
use teloxide::types::ChatId;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const USER: i64 = 42;
const OWNER: i64 = 999;

/// `cmd_handler` is a large async state machine; the runtime uses a 16 MiB
/// worker stack. Test threads default to 2 MiB.
fn on_large_stack<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    std::thread::Builder::new()
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

fn shared_config() -> SharedConfig {
    SharedConfig {
        owner_id: OWNER,
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

struct Fixture {
    _dir: tempfile::TempDir,
    db: Arc<Db>,
    server: MockServer,
    bot: Bot,
    deps: ScopedDispatch,
    cap: BotCapability,
    bot_ctx: Arc<BotContext>,
}

async fn fixture(bot_type: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Arc::new(Db::new(dir.path().join("cap.db").to_str().unwrap()).unwrap());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 1_700_000_000,
                "chat": {"id": USER, "type": "private", "first_name": "T"},
                "text": "sent"
            }
        })))
        .mount(&server)
        .await;
    let bot = Bot::new("42:placeholder").set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let shared = Arc::new(shared_config());
    let bot_ctx = Arc::new(BotContext {
        id: 1,
        key: bot_type.to_string(),
        bot: bot.clone(),
        owner_id: OWNER,
        shared: shared.clone(),
        db: db.clone(),
    });
    let bot_manager = Arc::new(BotManager {
        bots: vec![bot_ctx.clone()],
        primary: bot_ctx.clone(),
        db: db.clone(),
        identity: IdentityHandle::unconnected(),
    });
    let deps = ScopedDispatch {
        db: db.clone(),
        cfg: shared.as_ref().clone(),
        bot_manager,
        crypto: None,
        ai: None,
        registry: Arc::new(CommandRegistry::new()),
    };
    let cap = BotCapability::new(1, bot_type, bot_type);
    Fixture {
        _dir: dir,
        db,
        server,
        bot,
        deps,
        cap,
        bot_ctx,
    }
}

fn message(text: &str) -> Message {
    message_from(text, USER)
}

fn message_from(text: &str, from: i64) -> Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 7,
        "date": 1_700_000_000,
        "chat": {"id": from, "type": "private", "first_name": "T"},
        "from": {"id": from, "is_bot": false, "first_name": "T", "username": "tester"},
        "text": text
    }))
    .expect("test Message must deserialize")
}

/// A document message whose caption carries the /upload command, as a real
/// Telegram upload arrives.
fn document_message(caption: &str) -> Message {
    serde_json::from_value(serde_json::json!({
        "message_id": 8,
        "date": 1_700_000_000,
        "chat": {"id": USER, "type": "private", "first_name": "T"},
        "from": {"id": USER, "is_bot": false, "first_name": "T", "username": "tester"},
        "caption": caption,
        "document": {
            "file_id": "BAADBAADBAADmQ",
            "file_unique_id": "unique-share-file",
            "file_name": "clip.mp4",
            "mime_type": "video/mp4",
            "file_size": 1234
        }
    }))
    .expect("test document Message must deserialize")
}

async fn raw_bodies(f: &Fixture) -> Vec<String> {
    f.server
        .received_requests()
        .await
        .expect("mock Bot API requests")
        .iter()
        .map(|req| String::from_utf8_lossy(&req.body).into_owned())
        .collect()
}

async fn api_paths(f: &Fixture) -> Vec<String> {
    f.server
        .received_requests()
        .await
        .expect("mock Bot API requests")
        .iter()
        .map(|req| req.url.path().to_string())
        .collect()
}

async fn run_document(f: Fixture, caption: &str, cmd: Command) -> (Fixture, Vec<String>) {
    let dialogue = Dialogue::new(InMemStorage::<State>::new().erase(), ChatId(USER));
    dispatch_scoped(
        f.bot.clone(),
        document_message(caption),
        cmd,
        dialogue,
        &f.cap,
        &f.deps,
        f.bot_ctx.clone(),
    )
    .await
    .expect("scoped document dispatch must succeed");
    let out = texts(&f).await;
    (f, out)
}

async fn run_from(f: Fixture, text: &str, from: i64, cmd: Command) -> (Fixture, Vec<String>) {
    let dialogue = Dialogue::new(InMemStorage::<State>::new().erase(), ChatId(from));
    dispatch_scoped(
        f.bot.clone(),
        message_from(text, from),
        cmd,
        dialogue,
        &f.cap,
        &f.deps,
        f.bot_ctx.clone(),
    )
    .await
    .expect("scoped dispatch must succeed");
    let out = texts(&f).await;
    (f, out)
}

async fn texts(f: &Fixture) -> Vec<String> {
    f.server
        .received_requests()
        .await
        .expect("mock Bot API requests")
        .iter()
        .map(|req| {
            // sendDocument uses multipart/form-data, not JSON: it carries no
            // `text` field and must not break the text collector.
            serde_json::from_slice::<serde_json::Value>(&req.body)
                .ok()
                .and_then(|body| body["text"].as_str().map(str::to_string))
                .unwrap_or_default()
        })
        .collect()
}

async fn run(f: Fixture, text: &str, cmd: Command) -> (Fixture, Vec<String>) {
    let dialogue = Dialogue::new(InMemStorage::<State>::new().erase(), ChatId(USER));
    dispatch_scoped(
        f.bot.clone(),
        message(text),
        cmd,
        dialogue,
        &f.cap,
        &f.deps,
        f.bot_ctx.clone(),
    )
    .await
    .expect("scoped dispatch must succeed");
    let out = texts(&f).await;
    (f, out)
}

static WEB_URL_ENV_LOCK: Mutex<()> = Mutex::new(());

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

#[test]
fn service_bot_lists_real_services_from_the_shop_catalogue() {
    on_large_stack(async {
        let f = fixture("service").await;
        f.db.upsert_seller(USER, Some("tester"), None);
        f.db.create_service(USER, "Consulting", "10", "USDT", 60, None)
            .expect("service must be created");
        let (_, out) = run(f, "/services", Command::Services).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("Consulting"), "{out:?}");
    });
}

#[test]
fn web_app_bot_issues_a_one_time_dashboard_claim_link() {
    let _lock = WEB_URL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _env = WebUrlEnvGuard::set(Some("http://127.0.0.1:8080"));
    on_large_stack(async {
        let f = fixture("web_app").await;
        let (f, out) = run(f, "/becomeseller", Command::BecomeSeller).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            out[0].contains("http://127.0.0.1:8080/setup/claim?token="),
            "{out:?}"
        );
        assert!(!out[0].to_lowercase().contains("password:"), "{out:?}");
        assert!(f.db.get_seller(USER).is_some(), "seller must be created");
    });
}

#[test]
fn reply_bot_records_a_real_support_ticket_reply() {
    on_large_stack(async {
        let f = fixture("reply_bot").await;
        f.db.upsert_seller(USER, Some("tester"), None);
        let ticket = f.db.create_ticket(USER, 555, "Question");
        let (f, out) = run(
            f,
            "/reply 1 | We can help",
            Command::Reply(format!("{ticket} | We can help")),
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(
            out[0].contains(&format!("Replied to ticket #{ticket}")),
            "{out:?}"
        );
        let messages = f.db.list_ticket_messages(ticket);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].1, "seller");
        assert_eq!(messages[0].3, "We can help");
    });
}

#[test]
fn scoped_bot_does_not_run_shop_only_commands() {
    on_large_stack(async {
        let f = fixture("service").await;
        let (_, out) = run(f, "/post hello", Command::Post("hello".into())).await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("Available here:"), "{out:?}");
        assert!(out[0].contains("/services"), "{out:?}");
        assert!(!out[0].contains("Default target channel"), "{out:?}");
    });
}

#[cfg(feature = "movie_bot")]
#[test]
fn movie_bot_lists_and_searches_a_real_catalogue() {
    on_large_stack(async {
        let f = fixture("movie_bot").await;
        f.db.upsert_seller(USER, Some("tester"), None);
        let (f, out) = run(
            f,
            "/productadd Night Shift | 5 | USDT | digital_file | BAADBAADFILE | 3",
            Command::ProductAdd("Night Shift | 5 | USDT | digital_file | BAADBAADFILE | 3".into()),
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("Product 'Night Shift' added"), "{out:?}");
        assert_eq!(f.db.list_products(Some(USER)).len(), 1);

        let (f, out) = run(f, "/shop", Command::Shop).await;
        let shop = out.last().expect("shop reply");
        assert!(shop.contains("Night Shift"), "{out:?}");

        let (_, out) = run(f, "/find night", Command::Find("night".into())).await;
        let find = out.last().expect("search reply");
        assert!(find.contains("Night Shift"), "{out:?}");
        assert!(find.contains("5 USDT"), "{out:?}");
        assert!(!find.contains("BAADBAADFILE"), "payload must stay hidden");
    });
}

#[cfg(feature = "movie_bot")]
#[test]
fn movie_bot_delivery_resends_a_digital_file_after_payment() {
    on_large_stack(async {
        let f = fixture("movie_bot").await;
        f.db.add_product(
            USER,
            "Night Shift",
            "5",
            "USDT",
            "digital_file",
            Some("BAADBAADFILE"),
            None,
        )
        .expect("product must be created");
        daybook::services::deliver::deliver(
            &f.bot,
            USER,
            "digital_file",
            "Night Shift",
            Some("BAADBAADFILE"),
            &f.db,
            USER,
            Some(1),
        )
        .await
        .expect("delivery must succeed");
        let paths = api_paths(&f).await;
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert!(paths[0].ends_with("/SendDocument"), "{paths:?}");
        let raw = raw_bodies(&f).await;
        assert!(
            raw[0].contains("BAADBAADFILE"),
            "the file_id must be re-sent: {raw:?}"
        );
    });
}

#[test]
fn file_share_upload_grant_and_download_enforce_access_control() {
    on_large_stack(async {
        let f = fixture("file_share").await;
        f.db.upsert_seller(USER, Some("tester"), None);
        let (f, out) = run_document(
            f,
            "/upload clips | holiday cut",
            Command::FileUpload("clips | holiday cut".into()),
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("Shared 'clips'"), "{out:?}");
        let shares = f.db.list_file_shares(Some(USER), Some(1));
        assert_eq!(shares.len(), 1);
        let (share_id, seller, bot_id, file_id, name, _desc, token, _active) = shares[0].clone();
        assert_eq!(seller, USER);
        assert_eq!(bot_id, 1);
        assert_eq!(file_id, "BAADBAADBAADmQ");
        assert_eq!(name, "clips");

        // A stranger without a grant is refused and the attempt is audited.
        let other = 777;
        let (f, out) = run_from(
            f,
            &format!("/getfile {token}"),
            other,
            Command::FileGet(token.clone()),
        )
        .await;
        assert!(
            out.last().is_some_and(|t| t.contains("do not have access")),
            "{out:?}"
        );
        assert!(!f.db.file_share_grant_exists(share_id, other));

        // The seller grants access; the same user can now download the file.
        let (f, out) = run(
            f,
            &format!("/share {share_id} {other}"),
            Command::FileShare(format!("{share_id} {other}")),
        )
        .await;
        assert!(
            out.last().is_some_and(|t| t.contains("can now download")),
            "{out:?}"
        );
        assert!(f.db.file_share_grant_exists(share_id, other));

        let (f, _out) = run_from(
            f,
            &format!("/getfile {token}"),
            other,
            Command::FileGet(token.clone()),
        )
        .await;
        let paths = api_paths(&f).await;
        assert!(
            paths.iter().any(|p| p.ends_with("/SendDocument")),
            "{paths:?}"
        );
        let raw = raw_bodies(&f).await;
        assert!(
            raw.iter().any(|b| b.contains(&file_id)),
            "the granted user must receive the stored file_id"
        );
        let audit = f.db.list_audit_log(100);
        assert!(audit.iter().any(|r| r.3 == "file_share.denied"));
        assert!(audit.iter().any(|r| r.3 == "file_share.grant"));
        assert!(audit.iter().any(|r| r.3 == "file_share.download"));
    });
}

#[test]
fn automation_rule_creation_trigger_and_audit() {
    on_large_stack(async {
        let f = fixture("automation").await;
        f.db.upsert_seller(USER, Some("tester"), None);
        let (f, out) = run(
            f,
            "/ruleadd hello | Hi, how can we help?",
            Command::RuleAdd("hello | Hi, how can we help?".into()),
        )
        .await;
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("Rule #1 added"), "{out:?}");
        let rules = f.db.list_automation_rules(Some(USER), Some(1), true);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].3, "hello");

        // Plain text on an automation bot runs the real trigger path.
        handle_scoped_plain(&f.bot, &message("hello there"), &f.cap, &f.bot_ctx)
            .await
            .expect("automation trigger must succeed");
        let out = texts(&f).await;
        assert!(
            out.iter().any(|t| t.contains("Hi, how can we help?")),
            "{out:?}"
        );
        let audit = f.db.list_audit_log(100);
        assert!(audit.iter().any(|r| r.3 == "automation.fire"), "{audit:?}");

        // A non-matching message fires nothing new.
        let (f, _) = run(f, "/rules", Command::Rules).await;
        let before = f.db.list_audit_log(100).len();
        handle_scoped_plain(&f.bot, &message("nothing relevant"), &f.cap, &f.bot_ctx)
            .await
            .expect("non-matching text must be a no-op");
        assert_eq!(f.db.list_audit_log(100).len(), before);
    });
}

#[test]
fn newly_live_scoped_bots_cannot_reach_shop_only_commands() {
    on_large_stack(async {
        #[allow(unused_mut)] // extended only in a movie_bot build
        let mut cases = vec![
            ("file_share", Command::Files),
            ("automation", Command::Rules),
        ];
        #[cfg(feature = "movie_bot")]
        cases.push(("movie_bot", Command::Shop));
        for (bot_type, allowed) in cases {
            let f = fixture(bot_type).await;
            let (_, out) = run(f, "/post hello", Command::Post("hello".into())).await;
            assert_eq!(out.len(), 1, "{bot_type}: {out:?}");
            assert!(out[0].contains("Available here:"), "{bot_type}: {out:?}");
            let cap = BotCapability::new(1, bot_type, bot_type);
            assert!(cap.allows_command(&allowed), "{bot_type}");
            assert!(!cap.allows_command(&Command::Post("hello".into())));
        }
    });
}
