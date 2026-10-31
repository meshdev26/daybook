// Channel scoping contracts: owner/seller channel panels, the default
// channel rules, and the composer dropdown.
use super::*;
use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_default_remove_scoping() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let app = router(state);
    owner_channel_scoping(app.clone(), owner, db.clone()).await;
    seller_channel_scoping(app, seller, db).await;
}

async fn owner_channel_scoping(app: Router, owner: String, db: Arc<crate::db::Db>) {
    // Owner adds to the global set (seller_id 0).
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/add", "channel=@one", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/add", "channel=@two", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());

    // Set default; the page shows the badge and hides Remove on that row.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/default", "channel=@one", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(db.default_channel().as_deref(), Some("@one"));
    let resp = app
        .clone()
        .oneshot(authed_req("/channels", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("@one") && body.contains("@two"), "{body}");
    assert!(body.contains("default"), "{body}");

    // Removing the default is refused, inline, and removes nothing.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/remove", "channel=@one", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "inline error, not PRG");
    let body = body_string(resp).await;
    assert!(body.contains("cannot be removed"), "{body}");
    assert_eq!(db.default_channel().as_deref(), Some("@one"));

    // A non-default row can take over, after which the old default can
    // be removed.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/default", "channel=@two", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/remove", "channel=@one", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(db.default_channel().as_deref(), Some("@two"));
}

async fn seller_channel_scoping(app: Router, seller: String, db: Arc<crate::db::Db>) {
    // Seller scope: adding sets the seller's own channel, removing
    // touches only their own row, and the default endpoint is refused.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/add", "channel=@mine", &seller))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(db.seller_channel(42).as_deref(), Some("@mine"));
    // The seller's panel shows their bot-managed channel read-only.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("@mine"), "{body}");
    assert!(body.contains("bot-managed"), "{body}");
    assert!(
        !body.contains("Global channels"),
        "seller must not see the owner's panel: {body}"
    );
    let resp = app
        .clone()
        .oneshot(post_authed_req("/channels/remove", "channel=@two", &seller))
        .await
        .unwrap();
    // The scoped writer refuses a cross-scope remove honestly: the page
    // re-renders with the refusal and the global row survives.
    assert_eq!(resp.status(), StatusCode::OK, "inline error, not PRG");
    let body = body_string(resp).await;
    assert!(body.contains("not yours to remove"), "{body}");
    assert!(
        db.default_channel().as_deref() == Some("@two"),
        "seller remove must stay scoped to their own rows"
    );
    let resp = app
        .oneshot(post_authed_req(
            "/channels/default",
            "channel=@mine",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// MANAGEMENT_DISCLOSURE_SPEC §3: a bot-scoped view whose global rows resolve
/// elsewhere says so honestly; a genuinely empty set keeps the original copy.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_scoped_empty_state_names_the_scoping_honestly() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    // No bot tokens exist, so every channel resolves to the placeholder bot
    // id 0 — any non-zero bot filter empties the list without a real change.
    let bot_id = db.create_bot("secondary", 1, None, "");
    let token = owner_session(&state).await;
    db.set_web_session_selected_bot(&sha256_hex(&token), Some(bot_id));
    let app = router(state);
    // Genuinely empty (zero global rows): the original copy stands even
    // though a bot is selected.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("No global channels yet."), "{body}");
    // One global row that resolves to another bot: the scoped view may not
    // claim the set is empty.
    db.add_channel("@one", 0).unwrap();
    let resp = app.oneshot(authed_req("/channels", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("No channels resolve to this bot yet."),
        "{body}"
    );
    assert!(!body.contains("No global channels yet."), "{body}");
}

/// MANAGEMENT_DISCLOSURE_SPEC §4: the add-form help collapses to one visible
/// line with the full text behind <details class="help-more"> (both forms).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_forms_collapse_long_help() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let app = router(state);
    // Owner/global form: one-liner + collapsed details with its own tail.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("@handle, t.me link, or -100 ID — the bot must be admin in the channel."),
        "{body}"
    );
    assert!(
        body.contains(r#"<details class="help-more"><summary>Which forms are accepted?</summary>"#),
        "{body}"
    );
    assert!(
        body.contains("Adding one does not change the default — use Set default."),
        "{body}"
    );
    // Seller form: same collapse, its own tail sentence.
    let resp = app.oneshot(authed_req("/channels", &seller)).await.unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("@handle, t.me link, or -100 ID — the bot must be admin in the channel."),
        "{body}"
    );
    assert!(body.contains("Adding one replaces the previous."), "{body}");
}

/// MANAGEMENT_DISCLOSURE_SPEC §5: the rename box never treats the identifier
/// as a name — a NOCASE no-op redirects with the honest info banner and
/// writes nothing; the pre-fill comes from the stored title, else empty with
/// the plain placeholder.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_rename_noop_is_honest_and_prefill_uses_the_title() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.add_channel("@one", 0).unwrap();
    db.set_channel_title("@one", Some("Tech Tips"));
    db.add_channel("@two", 0).unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    // A NOCASE match of the box against the identifier changes nothing.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/rename",
            "channel=@one&new=@ONE",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?renamed=1&same=1",
        "the guard redirects without touching the DB"
    );
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel.eq_ignore_ascii_case("@one"))
        .expect("@one survives the no-op rename");
    assert_eq!(row.channel, "@one", "identifier unchanged");
    assert_eq!(row.title.as_deref(), Some("Tech Tips"), "title unchanged");
    // The honest banner replaces the success banner.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels?renamed=1&same=1", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Nothing changed — that's still the channel's ID."),
        "{body}"
    );
    assert!(!body.contains("Channel renamed to"), "{body}");
    // The titled row pre-fills the stored title; the titleless one renders
    // empty with the plain placeholder (no `@` anywhere).
    assert!(
        body.contains(r#"name="new" value="Tech Tips" placeholder="""#),
        "{body}"
    );
    assert!(
        body.contains(r#"name="new" value="" placeholder="Friendly name, e.g. Tech Tips""#),
        "{body}"
    );
    // A real rename still lands the success banner.
    let resp = app
        .oneshot(post_authed_req(
            "/channels/rename",
            "channel=@two&new=TechTips",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?renamed=1&channel=TechTips"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn post_form_renders_channel_dropdown() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@one", 0).unwrap();
    state.db.add_channel("@two", 0).unwrap();
    state.db.set_seller_channel(42, "@mine");
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let app = router(state);
    // Owner scope (None = every table row): (default) first, then the
    // channels. The seller's bot-managed channel is NOT here — it lives
    // in the legacy sellers column, not the channels table.
    let resp = app
        .clone()
        .oneshot(authed_req("/dashboard", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(r#"<select name="channel">"#), "{body}");
    assert!(
        body.contains(r#"<option value="">(default)</option>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<option value="@one">@one</option>"#),
        "{body}"
    );
    assert!(
        !body.contains(r#"<option value="@mine">"#),
        "legacy seller channel must not leak into the owner's dropdown: {body}"
    );
    // Configured console shows no connect banner.
    assert!(
        !body.contains("The bot is not connected yet"),
        "configured state must not warn: {body}"
    );

    // Seller scope = the global namespace plus their own rows (the
    // landed list_channels contract), plus their merged legacy channel,
    // plus the (default) option.
    let resp = app
        .oneshot(authed_req("/dashboard", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"<option value="@mine">@mine</option>"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<option value="@one">@one</option>"#),
        "{body}"
    );
}
