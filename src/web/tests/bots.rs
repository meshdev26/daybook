// Bot console surfaces: /bot/select, the Bots management page (save /
// disable / delete / add wizard / snippet fallback), the bot-scope switcher
// and the per-page bot-scope scoping contracts.
use super::*;
use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bot_select_persists_selection_and_reset_for_owner() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    assert!(bot_id > 0, "create_bot returns the row id");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        db.get_web_session(&token).unwrap().3,
        Some(bot_id),
        "selection persists against the session row"
    );
    // Empty value resets to primary/all.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/bot/select", "bot_id=", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(db.get_web_session(&token).unwrap().3, None);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bot_select_refuses_disabled_and_unknown_bots() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("disabled", 1, None, "");
    state.db.set_bot_enabled(bot_id, false);
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "a forged product id is refused"
    );
    assert_eq!(
        db.get_web_session(&token).unwrap().3,
        None,
        "a disabled bot is not selectable and must not widen the view"
    );
    let resp = app
        .clone()
        .oneshot(post_authed_req("/bot/select", "bot_id=424242", &token))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "an unknown id is refused"
    );
    assert_eq!(db.get_web_session(&token).unwrap().3, None);
}

/// IA amendment §7: a forged type or seller id on a scope change is refused
/// with 403. The shipped form has no such fields (the Type facet is
/// client-side only), so their presence is always an attack.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bot_select_rejects_forged_type_and_seller_scope() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    for body in [
        format!("bot_id={bot_id}&seller_id=999"),
        format!("bot_id={bot_id}&product_type=movie_bot"),
        "bot_id=424242&seller_id=999".to_string(),
    ] {
        let resp = app
            .clone()
            .oneshot(post_authed_req("/bot/select", &body, &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "body={body}");
        assert_eq!(db.get_web_session(&token).unwrap().3, None);
    }
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bot_select_rejects_pinned_roles() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = seller_session(&state, 42).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        db.get_web_session(&token).unwrap().3,
        None,
        "a pinned session keeps no selection"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_page_lists_for_owner_and_403s_for_seller() {
    let (state, _dir) = make_state().await;
    state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let seller = seller_session(&state, 7).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/bots", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("secondary"), "{body}");
    assert!(body.contains("multi-bot management"), "{body}");
    let resp = app
        .clone()
        .oneshot(authed_req("/bots", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_save_updates_settings_and_sets_restart_required() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&owner_user_id=55&default_channel_id=9&enabled=on"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    // Boot-loaded DB row: a successful save flags the restart (F3.4/R1)
    // and lands on the ?saved=1 banner, never a silent redirect. The
    // redirect also names the saved card so the page re-opens it
    // (MANAGEMENT_DISCLOSURE_SPEC §2b).
    assert_eq!(
        resp.headers().get("location").unwrap(),
        format!("/bots?saved=1&bot={bot_id}").as_str(),
        "the save PRG carries the saved banner and the open-card param"
    );
    assert_eq!(db.get_config("restart_required").as_deref(), Some("1"));
    let b = db.get_bot(bot_id).unwrap();
    assert_eq!(b.owner_user_id, 55);
    assert_eq!(b.default_channel_id, Some(9));
    assert!(b.enabled);
    // An unchecked checkbox arrives as no field at all — that must
    // disable the bot rather than be ignored.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&owner_user_id=55"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(!db.get_bot(bot_id).unwrap().enabled);
    // The ?saved=1 banner names the restart consequence (F3.4).
    let resp = app
        .oneshot(authed_req("/bots?saved=1", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Bot settings saved — they apply on restart."),
        "{body}"
    );
}

/// MANAGEMENT_DISCLOSURE_SPEC §2: bot cards are collapsed `<details>`
/// accordions — the DB row id stays a deep-link anchor but is no longer
/// printed as a label, and `?saved=1&bot=N` opens exactly the saved card.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_page_cards_collapse_and_saved_param_opens_the_saved_card() {
    let (state, _dir) = make_state().await;
    let a = state.db.create_bot("alpha", 1, None, "");
    let b = state.db.create_bot("beta", 1, None, "");
    let token = owner_session(&state).await;
    let app = router(state);
    // Collapsed by default: no card carries an open attribute, the summary
    // hint is present, and the id never prints as `#N`.
    let resp = app
        .clone()
        .oneshot(authed_req("/bots", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"<details class="panel bot-card""#),
        "{body}"
    );
    assert!(
        body.contains("Expand to edit owner, default channel, or enabled state"),
        "{body}"
    );
    assert!(!body.contains(r#"<span class="mono">#"#), "{body}");
    assert!(
        !body.contains(&format!(r#"bot-card" id="bot-{a}" open>"#)),
        "{body}"
    );
    assert!(
        !body.contains(&format!(r#"bot-card" id="bot-{b}" open>"#)),
        "{body}"
    );
    // ?saved=1&bot=N opens exactly that one card.
    let resp = app
        .oneshot(authed_req(&format!("/bots?saved=1&bot={b}"), &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(&format!(r#"bot-card" id="bot-{b}" open>"#)),
        "{body}"
    );
    assert!(
        !body.contains(&format!(r#"bot-card" id="bot-{a}" open>"#)),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_save_failure_rerenders_with_nothing_changed_banner() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    // A malformed bot id: no silent redirect — the Bots page re-renders in
    // place with a focused banner that says nothing was changed (F3.4/R5).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            "bot_id=oops&owner_user_id=1",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Nothing was changed."), "{body}");
    assert!(body.contains("data-focus-on-load"), "{body}");
    // A well-formed but unknown bot id gets the same honest banner instead
    // of a dead-end 404.
    let resp = app
        .oneshot(post_authed_req(
            "/bots",
            "bot_id=987654&owner_user_id=1",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Nothing was changed."), "{body}");
}

/// IA amendment §3.3: the existing /bots edit form gains a shared Product
/// type field and a closed-by-default Advanced disclosure for per-type
/// settings — no new route, no other path changed.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_page_has_product_type_field_and_closed_advanced() {
    let (state, _dir) = make_state().await;
    let id = state.db.create_bot("cinema", 1, None, "");
    state.db.set_bot_type(id, "movie_bot").unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/bots", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Product type"), "{body}");
    assert!(body.contains(r#"name="bot_type""#), "{body}");
    assert!(body.contains(r#"<details class="advanced""#), "{body}");
    assert!(body.contains("<summary>Advanced</summary>"), "{body}");
    assert!(body.contains(r#"data-product-type="movie_bot""#), "{body}");
    // A stored type from config remains selectable even when its capability
    // was not compiled into this build.
    assert!(
        body.contains(r#"<option value="movie_bot" selected>Movie bots</option>"#),
        "{body}"
    );
    assert!(
        !body.contains(r#"<details class="advanced" data-product-type="movie_bot" open>"#),
        "Advanced must be closed by default: {body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_save_rejects_unknown_product_type_without_changing_row() {
    let (state, _dir) = make_state().await;
    let id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            "bot_id=&owner_user_id=1&bot_type=not_a_type",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // A well-formed id with an unknown type is rejected in place; the row is
    // untouched. (The malformed id above is the existing "nothing changed" path.)
    let resp = app
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={id}&owner_user_id=1&bot_type=not_a_type"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Unknown product type."), "{body}");
    assert_eq!(db.get_bot(id).unwrap().bot_type, "shop_bot");
}

/// Display-name cards: a saved name titles the card; an empty name falls
/// back to the key with the muted "no display name set" hint beside it.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_page_card_title_prefers_name_and_hints_on_fallback() {
    let (state, _dir) = make_state().await;
    let _named = state.db.create_bot("named-bot", 1, None, "Shop Bot");
    let _plain = state.db.create_bot("plain-bot", 1, None, "");
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/bots", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("<h3>Shop Bot</h3>"), "{body}");
    assert!(
        body.contains(r#"<h3>plain-bot <span class="help">no display name set</span></h3>"#),
        "{body}"
    );
    // The edit form echoes the current name and carries the 64-char cap.
    assert!(body.contains(r#"value="Shop Bot""#), "{body}");
    assert!(body.contains(r#"name="name" value="""#), "{body}");
    assert!(body.contains(r#"maxlength="64""#), "{body}");
    assert!(
        body.contains("Shown in the console instead of the bot key. Leave blank to use the key."),
        "{body}"
    );
}

/// bots_save persists the display name and keeps the saved-banner PRG; the
/// reload titles the card with it, and a blank save clears it back to the
/// key fallback.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_save_persists_display_name_and_blank_clears_it() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&name=My+Shop+Bot&owner_user_id=55"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        format!("/bots?saved=1&bot={bot_id}").as_str(),
        "the save PRG keeps its banner and open-card redirect"
    );
    assert_eq!(db.get_bot(bot_id).unwrap().name, "My Shop Bot");
    let resp = app
        .clone()
        .oneshot(authed_req(&format!("/bots?saved=1&bot={bot_id}"), &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("<h3>My Shop Bot</h3>"), "{body}");
    assert!(!body.contains("no display name set"), "{body}");
    // A blank field clears the name: the key fallback returns.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&name=&owner_user_id=55"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(db.get_bot(bot_id).unwrap().name, "");
    let resp = app.oneshot(authed_req("/bots", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("no display name set"), "{body}");
}

/// The 64-char cap is enforced in the console layer before any write: over
/// the cap the focused error renders and neither name nor owner changes;
/// exactly 64 is accepted.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_save_rejects_over_64_char_display_name_without_writing() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let too_long = "x".repeat(65);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&name={too_long}&owner_user_id=55"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("Display name must be at most 64 characters. Nothing was changed."),
        "{body}"
    );
    assert!(body.contains("data-focus-on-load"), "{body}");
    // Nothing was written — the co-submitted owner change is discarded too.
    let b = db.get_bot(bot_id).unwrap();
    assert_eq!(b.name, "");
    assert_eq!(b.owner_user_id, 1, "no DB write happened at all");
    // Exactly 64 characters is accepted.
    let ok = "x".repeat(64);
    let resp = app
        .oneshot(post_authed_req(
            "/bots",
            &format!("bot_id={bot_id}&name={ok}&owner_user_id=55"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let b = db.get_bot(bot_id).unwrap();
    assert_eq!(b.name, ok);
    assert_eq!(b.owner_user_id, 55, "valid save applies the other fields");
}

/// The rail Product selector labels products by display name, falling back
/// to the key; the option value stays the numeric bot id and each option
/// carries its `data-type` for the client-side type facet.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn rail_bot_selector_labels_use_display_name() {
    let (state, _dir) = make_state().await;
    let named = state.db.create_bot("named-bot", 1, None, "Shop Bot");
    let plain = state.db.create_bot("plain-bot", 1, None, "");
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(&format!(
            r#"<option value="{named}" data-type="shop_bot">Shop Bot</option>"#
        )),
        "{body}"
    );
    assert!(
        body.contains(&format!(
            r#"<option value="{plain}" data-type="shop_bot">plain-bot</option>"#
        )),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_add_page_gates_step2_behind_validation() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/bots/add", &token)).await.unwrap();
    let body = body_string(resp).await;
    // Wizard idiom step lines (F3.1/F3.8); Step 2 is a locked teaser — the
    // form itself renders only after a validated token.
    assert!(
        body.contains("Step 1 of 2 — prove the token works"),
        "{body}"
    );
    assert!(body.contains("Step 2 of 2"), "{body}");
    assert!(!body.contains(r#"name="token_env""#), "{body}");
    assert!(!body.contains(r#"name="owner_user_id""#), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_add_error_rerender_preserves_typed_values() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    // A nickname that fails the slug rule: the error page re-renders Step 2
    // in place with the typed values (R5), focused alert on top.
    let resp = app
        .oneshot(post_authed_req(
            "/bots/add",
            "key=BAD%20KEY&username=@shopbot&owner_user_id=42&token_env=BOT_MAIN",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("data-focus-on-load"), "{body}");
    assert!(
        body.contains("The bot nickname must be 1-32 lowercase letters, numbers, or dashes"),
        "{body}"
    );
    assert!(body.contains(r#"value="BAD KEY""#), "{body}");
    assert!(body.contains(r#"value="@shopbot""#), "{body}");
    assert!(body.contains(r#"value="42""#), "{body}");
    assert!(body.contains(r#"value="BOT_MAIN""#), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_add_without_config_file_auto_creates_config() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    // Hermetic: the auto-create path resolves data_dir() (cwd-relative, so a
    // repo checkout with a real data/bots.toml would read as a duplicate)
    // and persists BOTS_CONFIG through the shared .env writer. Point both at
    // test-owned paths so the run neither reads the developer's data dir nor
    // rewrites the real .env. SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("DATABASE_DIR", dir.path()) };
    unsafe { std::env::set_var("ENV_FILE", dir.path().join("test.env")) };
    // No BOTS_CONFIG configured (the default install): the console now
    // auto-creates a bots.toml next to the data dir and persists BOTS_CONFIG
    // into .env, so the add succeeds instead of dead-ending on a snippet.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots/add",
            "key=main&name=Main&owner_user_id=42&token_env=BOT_MAIN&default_channel=@shop",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/bots/add?added=main",
        "auto-create path reports success"
    );
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("DATABASE_DIR") };
    unsafe { std::env::remove_var("ENV_FILE") };
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_add_submit_writes_config_and_sets_restart_required() {
    let (state, dir) = make_state().await;
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    let _env = ENV_LOCK.lock().await;
    let cfg_path = dir.path().join("bots.toml");
    // SAFETY: serialized by ENV_LOCK; no other code reads BOTS_CONFIG
    // concurrently in the test binary.
    unsafe { std::env::set_var("BOTS_CONFIG", &cfg_path) };
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bots/add",
            "key=main&owner_user_id=42&token_env=BOT_MAIN",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/bots/add?added=main"
    );
    // A config write is read once at boot: the restart flag must be set
    // (checklist item 8 / R1).
    assert_eq!(db.get_config("restart_required").as_deref(), Some("1"));
    let toml_text = std::fs::read_to_string(&cfg_path).unwrap();
    assert!(toml_text.contains("key = \"main\""), "{toml_text}");
    assert!(
        toml_text.contains("token_env = \"BOT_MAIN\""),
        "{toml_text}"
    );
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("BOTS_CONFIG") };
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_disable_and_delete_set_restart_required_and_name_the_key() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    // Disable: reversible, so a secondary-styled button and a key-addressed
    // warn banner; the boot-loaded row flags the restart (F3.5/R1).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &format!("/bots/{bot_id}/disable"),
            "",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/bots?disabled=secondary",
        "the redirect addresses the bot by key, not numeric id"
    );
    assert_eq!(db.get_config("restart_required").as_deref(), Some("1"));
    let resp = app
        .clone()
        .oneshot(authed_req("/bots?disabled=secondary", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("disabled — it stops responding after the next restart."),
        "{body}"
    );
    // Delete: key-addressed banner that states what reappears on restart.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &format!("/bots/{bot_id}/delete"),
            "",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/bots?deleted=secondary"
    );
    assert_eq!(db.get_config("restart_required").as_deref(), Some("1"));
    let resp = app
        .oneshot(authed_req("/bots?deleted=secondary", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("deleted from the console."), "{body}");
    assert!(
        body.contains("Bots from your config file reappear on restart unless removed there."),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bots_disable_and_delete_unknown_id_change_nothing() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let db = state.db.clone();
    let app = router(state);
    for uri in ["/bots/424242/disable", "/bots/424242/delete"] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(uri, "", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let body = body_string(resp).await;
        assert!(
            body.contains("That bot no longer exists — nothing was changed."),
            "{uri}: {body}"
        );
        assert!(!body.contains("disabled — it stops"), "{uri}: {body}");
        assert!(!body.contains("deleted from the console"), "{uri}: {body}");
    }
    // The honest no-op never flags a restart (P3-3).
    assert_eq!(db.get_config("restart_required"), None);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn shell_shows_bot_selector_for_owner_and_pinned_label_for_seller() {
    let (state, _dir) = make_state().await;
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    let seller = seller_session(&state, 5).await;
    let db = state.db.clone();
    let app = router(state);
    // Owner: a real select listing every enabled bot.
    let resp = app
        .clone()
        .oneshot(authed_req("/dashboard", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("scope-product"), "{body}");
    assert!(body.contains("secondary"), "{body}");
    assert!(body.contains("All products"), "{body}");
    // Bots rides in the Offerings rail group for the owner only
    // (BOT_SWITCHING_UX_SPEC §3); a seller's rail never carries it.
    assert!(body.contains(r#"href="/bots""#), "{body}");
    // Seller: pinned label, no switching control.
    let resp = app
        .clone()
        .oneshot(authed_req("/dashboard", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("bot-scope-pinned"), "{body}");
    assert!(!body.contains("scope-product"), "{body}");
    assert!(!body.contains(r#"href="/bots""#), "{body}");
    // Owner with a selection: the matching option is preselected.
    db.set_web_session_selected_bot(&sha256_hex(&token), Some(bot_id));
    let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(&format!(
            r#"<option value="{bot_id}" data-type="shop_bot" selected>"#
        )),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn effective_bot_resolution_covers_all_roles() {
    let (mut state, _dir) = make_state().await;
    let db = state.db.clone();
    // A two-bot manager: primary id 0, secondary id 7.
    let cfg = Arc::new(state.cfg.clone());
    let mk = |id: i64, key: &str| {
        Arc::new(crate::bots::BotContext {
            id,
            key: key.to_string(),
            bot: teloxide::Bot::new(format!("{id}:placeholder")),
            owner_id: 0,
            shared: cfg.clone(),
            db: db.clone(),
        })
    };
    let primary = mk(0, "primary");
    let second = mk(7, "secondary");
    state.bots = Arc::new(crate::bots::BotManager {
        bots: vec![primary, second],
        primary: mk(0, "primary"),
        db: db.clone(),
        identity: state.identity.clone(),
    });
    // Owner, no selection → primary.
    let (acc, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
    let owner = Session {
        account_id: acc,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    assert_eq!(effective_bot(&state, &owner).key, "primary");
    // Owner, selection → that bot.
    let owner_sel = Session {
        selected_bot_id: Some(7),
        ..owner.clone()
    };
    assert_eq!(effective_bot(&state, &owner_sel).key, "secondary");
    // Owner, selection pointing at an unconfigured bot → primary.
    let owner_bad = Session {
        selected_bot_id: Some(999),
        ..owner.clone()
    };
    assert_eq!(effective_bot(&state, &owner_bad).key, "primary");
    // Seller pinned to bot 7 → that bot.
    db.upsert_seller_bot_id(42, Some(7));
    let seller = Session {
        account_id: 42,
        role: "seller".to_string(),
        seller_id: Some(42),
        selected_bot_id: None,
    };
    assert_eq!(effective_bot(&state, &seller).key, "secondary");
    // Seller with no bot mapping → primary.
    db.upsert_seller_bot_id(43, None);
    let seller2 = Session {
        account_id: 43,
        seller_id: Some(43),
        ..seller.clone()
    };
    assert_eq!(effective_bot(&state, &seller2).key, "primary");
    // Admin with no bot row → primary (no admin bot setter exists yet).
    let admin = Session {
        account_id: 99,
        role: "admin".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    assert_eq!(effective_bot(&state, &admin).key, "primary");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn plans_page_shows_only_selected_bot_plans() {
    let (state, _dir) = make_state().await;
    // The primary bot is a real row in deployed databases; NULL-bot_id
    // sellers attribute to the lowest bot id, so both rows must exist
    // for the attribution to split the way production does.
    let _primary_id = state.db.create_bot("primary", 1, None, "");
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    state.db.upsert_seller_bot_id(11, Some(bot_id));
    state.db.upsert_seller_bot_id(22, None);
    state.db.add_plan(11, "BotTwoPlan", "1", "USDT", 7, None);
    state.db.add_plan(22, "PrimPlan", "2", "USDT", 7, None);
    let token = owner_session(&state).await;
    let app = router(state);
    // Default (no selection): both bots' plans are visible.
    let resp = app
        .clone()
        .oneshot(authed_req("/plans", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoPlan"), "{body}");
    assert!(body.contains("PrimPlan"), "{body}");
    // Select the secondary bot: only its sellers' plans remain.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app
        .clone()
        .oneshot(authed_req("/plans", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoPlan"), "{body}");
    assert!(!body.contains("PrimPlan"), "{body}");
    // Reset: both visible again.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/bot/select", "bot_id=", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/plans", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("PrimPlan"), "{body}");
}

/// web-bot-scoping N1: /dashboard honors the selected bot for the plans
/// panel, recent subscriptions, the default-channel readout and the
/// onboarding aggregates. Seller 11 lives on the secondary bot, seller 22
/// on the primary (NULL bot_id attributes to the lowest bot id).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn dashboard_honors_selected_bot_scope() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    // Two different defaults, so the readout names which bot resolved it.
    db.set_default_channel("@globalchan").unwrap();
    db.add_channel("@bchan", 0).unwrap();
    let bchan_id = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@bchan")
        .map(|c| c.id)
        .expect("@bchan row exists");
    let _primary_id = db.create_bot("primary", 1, None, "");
    let bot_id = db.create_bot("secondary", 1, Some(bchan_id), "");
    db.upsert_seller_bot_id(11, Some(bot_id));
    db.upsert_seller_bot_id(22, None);
    db.add_plan(11, "BotTwoPlan", "1", "USDT", 7, None);
    db.add_plan(22, "PrimPlan", "2", "USDT", 7, None);
    db.create_subscription(11, 501, None, "@subB", 30, None, None);
    db.create_subscription(22, 502, None, "@subA", 30, None, None);

    let token = owner_session(&state).await;
    let app = router(state);
    // No selection: both sellers' rows and the global default.
    let resp = app
        .clone()
        .oneshot(authed_req("/dashboard", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoPlan"), "{body}");
    assert!(body.contains("PrimPlan"), "{body}");
    assert!(body.contains("@subB"), "{body}");
    assert!(body.contains("@subA"), "{body}");
    // B1.2 (VISUAL_POLISH_SPEC): the default-channel readout is the .stat
    // component now — same string, prescribed markup.
    assert!(
        body.contains(r#"<span class="stat-value">@globalchan</span>"#),
        "{body}"
    );
    // Select the secondary bot: only seller 11's rows, that bot's default.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app
        .clone()
        .oneshot(authed_req("/dashboard", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoPlan"), "{body}");
    assert!(!body.contains("PrimPlan"), "{body}");
    assert!(body.contains("@subB"), "{body}");
    assert!(!body.contains("@subA"), "{body}");
    // B1.2 markup (see above) for the scoped readout and its absence.
    assert!(
        body.contains(r#"<span class="stat-value">@bchan</span>"#),
        "{body}"
    );
    assert!(
        !body.contains(r#"<span class="stat-value">@globalchan</span>"#),
        "{body}"
    );
    // Reset: everything is back, including the global default.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/bot/select", "bot_id=", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("PrimPlan"), "{body}");
    assert!(body.contains("@subA"), "{body}");
    // B1.2 (VISUAL_POLISH_SPEC): the default-channel readout is the .stat
    // component now — same string, prescribed markup.
    assert!(
        body.contains(r#"<span class="stat-value">@globalchan</span>"#),
        "{body}"
    );
}

/// web-bot-scoping N1: /codes lists only the selected bot's codes and
/// offers only its sellers' plans. Distinct prices mark the plans because
/// the picker labels options from `PlanRow.1`.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn codes_page_honors_selected_bot_scope() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let _primary_id = db.create_bot("primary", 1, None, "");
    let bot_id = db.create_bot("secondary", 1, None, "");
    db.upsert_seller_bot_id(11, Some(bot_id));
    db.upsert_seller_bot_id(22, None);
    db.add_plan(11, "BotTwoPlan", "7731", "USDT", 7, None);
    db.add_plan(22, "PrimPlan", "8842", "USDT", 7, None);
    db.create_code(11, None, Some(30), 1, None, 12);
    db.create_code(22, None, Some(30), 1, None, 12);
    let bot_code = db.list_codes(Some(11), 10)[0].0.clone();
    let prim_code = db.list_codes(Some(22), 10)[0].0.clone();
    assert_ne!(bot_code, prim_code, "codes are distinct rows");

    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/codes", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(&bot_code), "{body}");
    assert!(body.contains(&prim_code), "{body}");
    assert!(body.contains(r#"value="7731""#), "{body}");
    assert!(body.contains(r#"value="8842""#), "{body}");
    // Select the secondary bot: its code and its plan option only.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/codes", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(&bot_code), "{body}");
    assert!(!body.contains(&prim_code), "{body}");
    assert!(body.contains(r#"value="7731""#), "{body}");
    assert!(!body.contains(r#"value="8842""#), "{body}");
}

/// web-bot-scoping N1: /reviews narrows the product list (and therefore
/// the pending reviews) to the selected bot's sellers.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn reviews_page_honors_selected_bot_scope() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let _primary_id = db.create_bot("primary", 1, None, "");
    let bot_id = db.create_bot("secondary", 1, None, "");
    db.upsert_seller_bot_id(11, Some(bot_id));
    db.upsert_seller_bot_id(22, None);
    let bot_product = db
        .add_product(11, "BotTwoWidget", "1", "USDT", "digital_link", None, None)
        .unwrap();
    let prim_product = db
        .add_product(22, "PrimWidget", "2", "USDT", "digital_link", None, None)
        .unwrap();
    db.create_review(501, 11, bot_product, 5, Some("BotTwoReviewText"))
        .unwrap();
    db.create_review(502, 22, prim_product, 4, Some("PrimReviewText"))
        .unwrap();

    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/reviews", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoWidget"), "{body}");
    assert!(body.contains("PrimWidget"), "{body}");
    assert!(body.contains("BotTwoReviewText"), "{body}");
    assert!(body.contains("PrimReviewText"), "{body}");
    // Select the secondary bot: only its product and review row.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/reviews", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("BotTwoWidget"), "{body}");
    assert!(body.contains("BotTwoReviewText"), "{body}");
    assert!(!body.contains("PrimWidget"), "{body}");
    assert!(!body.contains("PrimReviewText"), "{body}");
}

/// web-bot-scoping N1: /analytics aggregates exactly the selected bot's
/// sellers — the other seller's top product disappears.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn analytics_page_honors_selected_bot_scope() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let _primary_id = db.create_bot("primary", 1, None, "");
    let bot_id = db.create_bot("secondary", 1, None, "");
    db.upsert_seller_bot_id(11, Some(bot_id));
    db.upsert_seller_bot_id(22, None);
    let bot_product = db
        .add_product(11, "BotTwoWidget", "1", "USDT", "digital_link", None, None)
        .unwrap();
    let prim_product = db
        .add_product(22, "PrimWidget", "2", "USDT", "digital_link", None, None)
        .unwrap();
    db.grant_product(11, 501, bot_product, "invB1").unwrap();
    db.grant_product(22, 502, prim_product, "invA1").unwrap();
    let bot_marker = format!(">Product {bot_product}<");
    let prim_marker = format!(">Product {prim_product}<");

    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/analytics", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(&bot_marker), "{body}");
    assert!(body.contains(&prim_marker), "{body}");
    // Select the secondary bot: only its product aggregates.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/analytics", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(&bot_marker), "{body}");
    assert!(!body.contains(&prim_marker), "{body}");
}

/// web-bot-scoping N1: /finance lists only the selected bot's payouts and
/// refunds. Create/approve/execute stay untouched.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn finance_page_honors_selected_bot_scope() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let _primary_id = db.create_bot("primary", 1, None, "");
    let bot_id = db.create_bot("secondary", 1, None, "");
    db.upsert_seller_bot_id(11, Some(bot_id));
    db.upsert_seller_bot_id(22, None);
    db.create_payout(11, "222.00", "USDT", Some("501"));
    db.create_payout(22, "111.00", "USDT", Some("502"));

    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/finance", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("222.00"), "{body}");
    assert!(body.contains("111.00"), "{body}");
    // Select the secondary bot: only seller 11's payout row.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let resp = app.oneshot(authed_req("/finance", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("222.00"), "{body}");
    assert!(!body.contains("111.00"), "{body}");
}
