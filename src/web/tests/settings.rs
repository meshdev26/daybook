// Settings surfaces: bot connection saves, password rotation, and the AI
// configuration/test pages.
use super::*;
use axum::http::StatusCode;
use tower::ServiceExt;

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_seller_refused() {
    let (state, _dir) = make_state().await;
    let token = seller_session(&state, 42).await;
    let app = router(state);
    for uri in ["/settings"] {
        let resp = app.clone().oneshot(authed_req(uri, &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
        assert!(
            body_string(resp)
                .await
                .contains("restricted to the console owner")
        );
    }
    for (uri, body) in [
        ("/settings/save", "owner_id=42"),
        ("/settings/password", "new_password=secret1"),
    ] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(uri, body, &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_save_blank_fields_change_nothing() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "bot_token=&owner_id=&default_channel=&crypto_token=",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Nothing to save"), "{body}");
    assert!(!env_path.exists(), "all-blank save must not create .env");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_save_writes_env_via_helper() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    // No bot token in the form (that path validates against Telegram);
    // the other fields go straight through the shared writer.
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "owner_id=42&default_channel=@shop",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Saved. The bot connects on restart."),
        "{body}"
    );
    let env = std::fs::read_to_string(&env_path).expect(".env written");
    assert!(env.contains("OWNER_ID=42"), "{env}");
    assert!(env.contains("DEFAULT_CHANNEL=@shop"), "{env}");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_save_rejects_invalid_token_without_writing() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "bot_token=123456:definitely-not-a-token",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    // Either Telegram rejected the token or the console could not reach it —
    // both honest verdicts share the "Nothing was saved." tail (F2.5/R5);
    // which one depends on the network, so only the tail is asserted here.
    assert!(body.contains("Nothing was saved."), "{body}");
    assert!(!env_path.exists(), "a rejected token must never reach .env");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_save_owner_error_names_the_fix() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    // Local-only failure (no Telegram involved): the F2.5 copy says what to
    // do and that nothing was saved.
    let resp = app
        .oneshot(post_authed_req("/settings/save", "owner_id=oops", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("data-focus-on-load"), "{body}");
    assert!(
        body.contains(
            "Owner ID must be the number @userinfobot replies with — for example 123456789."
        ),
        "{body}"
    );
    assert!(body.contains("Nothing was saved."), "{body}");
    assert!(!env_path.exists(), "a failed save must not create .env");
    clear_env_file_var();
}

// ---- SETTINGS_PER_BOT_TOKEN_SPEC §8.2: multi-bot settings (additive) ----

/// A multi-bot test state: the legacy `make_state()` with the boot snapshot
/// pointed at two config-file bots — `primary` resolves a token through the
/// named `BOT_PRIMARY` setting (configured), `proxy26_bot` has the named
/// `BOT_PROXY26_BOT` setting but no token (not set). `cfg` is a plain field,
/// so tests mutate the snapshot directly (§8.2 preamble); every token here
/// is the house fake, never a real-shaped literal.
#[tracing::instrument(skip_all)]
async fn make_state_multi_bot() -> (WebState, TempDir) {
    let (mut state, dir) = make_state().await;
    state.cfg.bots = vec![
        crate::config::BotConfig {
            key: "primary".to_string(),
            name: "Primary".to_string(),
            token: "test-token-placeholder".to_string(),
            owner_id: 1,
            default_channel: None,
            token_env: Some("BOT_PRIMARY".to_string()),
            bot_type: crate::config::DEFAULT_BOT_TYPE.to_string(),
        },
        crate::config::BotConfig {
            key: "proxy26_bot".to_string(),
            name: "Proxy26_Bot".to_string(),
            token: String::new(),
            owner_id: 0,
            default_channel: None,
            token_env: Some("BOT_PROXY26_BOT".to_string()),
            bot_type: crate::config::DEFAULT_BOT_TYPE.to_string(),
        },
    ];
    state.cfg.bots_legacy = false;
    (state, dir)
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_renders_one_field_per_bot() {
    let (state, _dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    state.db.create_bot("proxy26_bot", 0, None, "Proxy26_Bot");
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/settings", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    // One write-only field per DB bot, addressed by key — and no legacy
    // shadow field at all (mode exclusivity, §2).
    assert_eq!(body.matches(r#"name="bot_primary""#).count(), 1, "{body}");
    assert_eq!(
        body.matches(r#"name="bot_proxy26_bot""#).count(),
        1,
        "{body}"
    );
    assert!(!body.contains(r#"name="bot_token""#), "{body}");
    // House shape: the secret-field wrapper with the shared reveal toggle,
    // and the write-target var named in each row's help.
    assert!(
        body.contains(r#"id="settings-bot-token-primary""#),
        "{body}"
    );
    assert!(body.contains("secret-toggle"), "{body}");
    assert!(body.contains("<code>BOT_PRIMARY</code>"), "{body}");
    assert!(body.contains("<code>BOT_PROXY26_BOT</code>"), "{body}");
    // Badges read the boot-resolved tokens: configured vs not set (§3.3).
    assert!(
        body.contains(
            r#"<label for="settings-bot-token-primary">Bot token — Primary <span class="badge badge-success">configured</span></label>"#
        ),
        "{body}"
    );
    assert!(
        body.contains(
            r#"<label for="settings-bot-token-proxy26_bot">Bot token — Proxy26_Bot <span class="badge badge-warning">not set</span></label>"#
        ),
        "{body}"
    );
    // The §3.2 mode line, and the scoped honesty note for the two
    // legacy-env fields the file branch never reads.
    assert!(body.contains("Multi-bot mode."), "{body}");
    assert!(
        body.contains("these two fields only apply in single-bot mode."),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_inline_token_row_has_no_input() {
    let (mut state, _dir) = make_state_multi_bot().await;
    // An inline token in the config file has no setting name to write to
    // (token_env: None) — the row must offer no input at all.
    state.cfg.bots[0].token_env = None;
    state.db.create_bot("primary", 1, None, "Primary");
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/settings", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(!body.contains(r#"name="bot_primary""#), "{body}");
    assert!(
        body.contains("set directly in your bot config file — edit it there"),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_blank_save_writes_nothing() {
    let (state, dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    state.db.create_bot("proxy26_bot", 0, None, "Proxy26_Bot");
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "section=connection&bot_primary=&bot_proxy26_bot=&owner_id=&default_channel=&crypto_token=",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Nothing to save"), "{body}");
    assert!(!env_path.exists(), "all-blank save must not create .env");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_composes_right_vars() {
    let (mut state, _dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    state.db.create_bot("proxy26_bot", 0, None, "Proxy26_Bot");
    let mut form = HashMap::new();
    form.insert(
        "bot_primary".to_string(),
        "test-token-placeholder".to_string(),
    );
    form.insert("bot_proxy26_bot".to_string(), String::new());
    // Pure composition: exactly the non-blank state-A row becomes a write;
    // the blank row writes nothing and names no bot.
    let writes = token_env_updates(&state, &form).expect("compose succeeds");
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].var, "BOT_PRIMARY");
    assert_eq!(writes[0].value, "test-token-placeholder");
    assert_eq!(writes[0].bot_name, "Primary");
    // §5.2 guard: two rows resolving to the SAME var with different values
    // are rejected (dotenvy is first-wins; a duplicate line would lie).
    state.cfg.bots[1].token_env = Some("BOT_PRIMARY".to_string());
    let mut shared = HashMap::new();
    shared.insert(
        "bot_primary".to_string(),
        "test-token-placeholder".to_string(),
    );
    shared.insert("bot_proxy26_bot".to_string(), "another-fake".to_string());
    let err = token_env_updates(&state, &shared).expect_err("shared var rejected");
    assert!(err.contains("share the setting"), "{err}");
    assert!(err.contains("BOT_PRIMARY"), "{err}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_token_debug_redacts_value() {
    let (state, _dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    let mut form = HashMap::new();
    form.insert(
        "bot_primary".to_string(),
        "test-token-placeholder".to_string(),
    );
    let writes = token_env_updates(&state, &form).expect("compose succeeds");
    // L3 P3-1: the Debug render is a redaction surface — a future
    // `tracing::debug!(?writes)` can never leak the token value.
    let dbg = format!("{:?}", writes);
    assert!(dbg.contains("<redacted>"), "{dbg}");
    assert!(!dbg.contains("test-token-placeholder"), "{dbg}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_writer_replaces_and_preserves() {
    let (state, dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    let mut form = HashMap::new();
    form.insert(
        "bot_primary".to_string(),
        "test-token-placeholder".to_string(),
    );
    // The handler's managed set: exactly the composed writes (legacy keys
    // are blank here, so the set is just the one bot var).
    let writes = token_env_updates(&state, &form).expect("compose succeeds");
    let updates: Vec<(String, Option<String>)> = writes
        .iter()
        .map(|w| (w.var.clone(), Some(w.value.clone())))
        .collect();
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    std::fs::write(&env_path, "BOT_PRIMARY=stale-value\nWEB_BIND=0.0.0.0\n").unwrap();
    set_env_file_var(&env_path);
    let managed: Vec<(&str, Option<&str>)> = updates
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_deref()))
        .collect();
    crate::config::write_env_file_replacing(&managed).expect("write succeeds");
    // The managed key is REPLACED (exactly one line, stale value gone); the
    // foreign key survives verbatim.
    let env = std::fs::read_to_string(&env_path).unwrap();
    assert_eq!(env.matches("BOT_PRIMARY=").count(), 1, "{env}");
    assert!(env.contains("WEB_BIND=0.0.0.0"), "{env}");
    assert!(!env.contains("stale-value"), "{env}");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_invalid_token_writes_nothing() {
    let (state, dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    state.db.create_bot("proxy26_bot", 0, None, "Proxy26_Bot");
    let db = state.db.clone();
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "section=connection&bot_primary=123456:definitely-not-a-token",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    // The banner names the failing bot and keeps the shared nothing-saved
    // tail; which verdict (rejected vs unreachable) depends on the network.
    assert!(body.contains("Primary — "), "{body}");
    assert!(body.contains("Nothing was saved."), "{body}");
    assert!(!env_path.exists(), "a rejected token must never reach .env");
    assert!(
        db.get_config("restart_required").is_none(),
        "a rejected save must not set the restart flag"
    );
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_multi_bot_rejects_shadowed_legacy_field() {
    let (state, dir) = make_state_multi_bot().await;
    state.db.create_bot("primary", 1, None, "Primary");
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    // A stale bookmark or crafted POST must not resurrect the shadowed
    // legacy write (§5.6) — even alongside otherwise-valid fields.
    let resp = app
        .oneshot(post_authed_req(
            "/settings/save",
            "section=connection&bot_token=nonblank&owner_id=42",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Single-bot token fields are not used in multi-bot mode"),
        "{body}"
    );
    assert!(body.contains("Nothing was saved."), "{body}");
    assert!(!env_path.exists(), "the shadow write must stay dead");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_legacy_mode_renders_single_field_and_mode_line() {
    let (state, _dir) = make_state().await; // bots_legacy: true
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/settings", &token)).await.unwrap();
    let body = body_string(resp).await;
    // §4 byte-guard: the single legacy field plus exactly one mode line.
    assert!(
        body.contains("This console is in single-bot mode."),
        "{body}"
    );
    assert!(body.contains(r#"name="bot_token""#), "{body}");
    assert!(!body.contains("Multi-bot mode."), "{body}");
    // No per-bot rows: only the legacy field (the per-row ids carry a
    // `-` key suffix the single field never has; `name="bot_id"` is the
    // unrelated bot-scope select).
    assert!(!body.contains(r#"id="settings-bot-token-""#), "{body}");
    assert_eq!(body.matches(r#"name="bot_token""#).count(), 1, "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_logout_link_lives_in_management_not_rail() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/settings", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    // The rail Utilities strip now also carries a Sign out link, and the
    // Management section keeps its sign-out form.
    assert!(body.contains(r#"href="/logout""#), "{body}");
    assert!(body.contains(r#"action="/logout""#), "{body}");
    assert!(body.contains("Sign out"), "{body}");
    assert!(body.contains("Sign out of this console."), "{body}");
    // The new POST arm clears the session exactly like the GET arm.
    let resp = app
        .oneshot(post_authed_req("/logout", "", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set_cookie.contains("Max-Age=0"), "{set_cookie}");
    assert!(db.get_web_session(&token).is_none());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_password_changes_and_invalidates_other_sessions() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let kept = owner_session(&state).await;
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
    let other = db.create_web_session(acc_id, "owner", None, 7);
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/password",
            "new_password=fresh1",
            &kept,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Password changed. Other sessions were signed out."),
        "{body}"
    );
    // The caller's session survives; every other session of the account
    // is gone; the new password verifies.
    assert!(db.get_web_session(&kept).is_some());
    assert!(db.get_web_session(&other).is_none());
    assert!(db.verify_web_password("owner", "fresh1").await.is_some());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_owner_access_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/ai", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("AI"), "{body}");
    assert!(body.contains("AI provider"), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_seller_refused_403() {
    let (state, _dir) = make_state().await;
    let token = seller_session(&state, 42).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/ai", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_anon_redirects() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/ai")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_save_toggle_only_writes_toggle() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req("/ai/save", "", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Saved. AI settings apply on restart."),
        "{body}"
    );
    let env = std::fs::read_to_string(&env_path).expect(".env written");
    assert!(env.contains("AI_ENABLED=0"), "{env}");
    // non-toggle blanks must not be written
    assert!(!env.contains("AI_PROVIDER="), "{env}");
    assert!(!env.contains("AI_API_KEY="), "{env}");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_save_blank_non_toggle_keeps_current_no_clobber() {
    // submitting an invalid provider must not write .env
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let env_path = dir.path().join(".env");
    set_env_file_var(&env_path);
    let resp = app
        .oneshot(post_authed_req(
            "/ai/save",
            "ai_enabled=1&ai_provider=bogus",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("alert-error"), "{body}");
    assert!(!env_path.exists(), "rejected save must not touch .env");
    clear_env_file_var();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_test_no_key_returns_not_configured() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req("/ai/test", "ai_provider=openai", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("No API key configured."), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn ai_test_redacts_sentinel_key() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let sentinel = "sk-sentinel-SECRET-12345";
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(401).set_body_string(format!("bad key {} in body", sentinel)),
        )
        .mount(&server)
        .await;

    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let body = format!(
        "ai_provider=openai&ai_base_url={}&ai_api_key={}&ai_model=gpt-4o-mini",
        server.uri(),
        sentinel
    );
    let resp = app
        .oneshot(post_authed_req("/ai/test", &body, &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let out = body_string(resp).await;
    assert!(!out.contains(sentinel), "sentinel leaked: {out}");
    assert!(
        out.contains("\"ok\":false") || out.contains("\"ok\": false"),
        "{out}"
    );
}

#[test]
fn redact_ai_error_covers_key_url_userinfo() {
    let key = "sk-sentinel-SECRET-12345";
    let msg = format!(
        "request failed: key={} url=https://user:pass@example.com/v1/chat/completions?api_key={}",
        key, key
    );
    let out = redact_ai_error(&msg, key, "https://user:pass@example.com/v1");
    assert!(!out.contains(key), "key leaked: {out}");
    assert!(!out.contains("pass"), "password leaked: {out}");
    assert!(!out.contains("user:"), "userinfo leaked: {out}");
    // query string (containing the key) must be gone
    assert!(!out.contains("api_key="), "query leaked: {out}");
}
