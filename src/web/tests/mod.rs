// Console test suite, split by domain (P2-2b, move-only): whole test fns
// moved verbatim from the former tests.rs into the sibling modules below.
// Cross-domain helpers (states/peers used by more than one domain) live
// here as `pub(super)` so every child can reach them via `use super::*`.
pub use super::test_support::*;
use super::*;
use std::sync::Arc;
use tempfile::TempDir;

mod bots;
mod channels;
mod misc;
#[cfg(feature = "moderation")]
mod moderation;
mod nav;
mod settings;
mod setup;
mod subs;

/// A state that has CLOSED setup (setup_done=1) but created no owner —
/// the post-skip world, where /login is the claim form.
#[tracing::instrument(skip_all)]
pub(super) async fn make_unclaimed_state() -> (WebState, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Db::new(db_path.to_str().unwrap()).unwrap();
    db.set_config("setup_done", "1");
    let cfg = Config {
        owner_id: 0,
        db_path: db_path.to_str().unwrap().to_string(),
        crypto_token: None,
        ai_enabled: false,
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
    };
    let db = Arc::new(db);
    let bots = Arc::new(crate::bots::BotManager::placeholder(
        db.clone(),
        Arc::new(cfg.clone()),
        crate::identity::IdentityHandle::unconnected(),
    ));
    let state = WebState {
        db: db.clone(),
        cfg,
        bot: teloxide::Bot::new("0:dummy"),
        bots,
        identity: crate::identity::IdentityHandle::unconnected(),
        login_attempts: Arc::new(StdMutex::new(HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
        base_path: String::new(),
    };
    (state, dir)
}
