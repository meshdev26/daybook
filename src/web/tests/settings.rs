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
