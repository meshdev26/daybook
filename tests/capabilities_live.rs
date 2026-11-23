//! Inbound behavior of the live scoped capabilities. Each test drives the
//! real command handler (`dispatch_scoped` -> `cmd_handler`) against a mock
//! Bot API, so the reply comes from shipped code rather than capability copy.

use std::future::Future;
use std::sync::{Arc, Mutex};

use daybook::bots::{BotContext, BotManager};
use daybook::config::SharedConfig;
use daybook::db::Db;
use daybook::dialogue::State;
use daybook::handlers::capabilities::{BotCapability, ScopedDispatch, dispatch_scoped};
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
    serde_json::from_value(serde_json::json!({
        "message_id": 7,
        "date": 1_700_000_000,
        "chat": {"id": USER, "type": "private", "first_name": "T"},
        "from": {"id": USER, "is_bot": false, "first_name": "T", "username": "tester"},
        "text": text
    }))
    .expect("test Message must deserialize")
}

async fn texts(f: &Fixture) -> Vec<String> {
    f.server
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
