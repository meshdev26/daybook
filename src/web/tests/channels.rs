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
/// CHANNELS_MASTER_DETAIL_SPEC §3: both forms live on the dedicated
/// GET /channels/add page (URL-moved, copy byte-identical).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_forms_collapse_long_help() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let app = router(state);
    // Owner/global form: one-liner + collapsed details with its own tail.
    // (CHANNEL_ADD_REDESIGN_SPEC §3.1: the one-liner is now the per-visibility
    // public help; the visibility radio + adaptive identifier field carry the
    // rest, and the omnibus rules live behind the same help-more summary.)
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/add", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("@handle or a t.me link — the bot must be admin in the channel."),
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
    // §3.1: the unified visibility-aware form — radio group, adaptive
    // identifier field with its placeholder swap data, and the Title box.
    assert!(body.contains(r#"<fieldset class="visibility">"#), "{body}");
    assert!(body.contains("data-visibility-form"), "{body}");
    assert!(
        body.contains(r#"data-ph-public="@handle" data-ph-private="-1001234567890123""#),
        "{body}"
    );
    assert!(
        body.contains(r#"<input id="ch-title" name="title""#),
        "{body}"
    );
    // Seller form: same collapse, its own tail sentence — and the same
    // visibility radio but NO Title field (§3.2 honesty: a seller-typed
    // title would be silently dropped).
    let resp = app
        .oneshot(authed_req("/channels/add", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("@handle or a t.me link — the bot must be admin in the channel."),
        "{body}"
    );
    assert!(body.contains("Adding one replaces the previous."), "{body}");
    assert!(body.contains(r#"<fieldset class="visibility">"#), "{body}");
    assert!(
        !body.contains(r#"<input id="ch-title" name="title""#),
        "seller form must not offer a Title control: {body}"
    );
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
        "/channels/manage?channel=@one&renamed=1&same=1",
        "the guard redirects to the detail view without touching the DB"
    );
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel.eq_ignore_ascii_case("@one"))
        .expect("@one survives the no-op rename");
    assert_eq!(row.channel, "@one", "identifier unchanged");
    assert_eq!(row.title.as_deref(), Some("Tech Tips"), "title unchanged");
    // The honest banner replaces the success banner (CHANNEL_ADD_REDESIGN_SPEC
    // §5b: the guard copy speaks the address noun and points at Edit name) —
    // surfaced on the detail view (CHANNELS_MASTER_DETAIL_SPEC §6).
    let resp = app
        .clone()
        .oneshot(authed_req(
            "/channels/manage?channel=@one&renamed=1&same=1",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Nothing changed — that's still the channel's address."),
        "{body}"
    );
    assert!(!body.contains("Channel renamed to"), "{body}");
    // The titled row pre-fills the stored title in the Edit-name box; the
    // titleless one renders empty with the plain placeholder (§5a — the
    // name field posts to /channels/name, the address field to /channels/rename;
    // both controls now live on the detail page).
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@one", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(r#"name="name" value="Tech Tips""#), "{body}");
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@two", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"name="name" value="" placeholder="Friendly name, e.g. Wire Guard""#),
        "{body}"
    );
    // A real rename still lands the success banner — on the detail view.
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
        "/channels/manage?channel=TechTips&renamed=1"
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

/// CHANNEL_ADD_REDESIGN_SPEC §3.4/§8.2.1: the visibility radio is the truth,
/// the server is the enforcement — a Public add via a t.me link stores the
/// normalized @handle (badge reads public), a non-ID paste under Private and
/// a numeric paste under Public are refused with the fixed copy and store
/// nothing.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_visibility_validation_is_shape_aware() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let token = owner_session(&state).await;
    let app = router(state);
    // Public via t.me link: stored as the @handle, badge reads public.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=public&channel=t.me/wireguard",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("added=1"), "{loc}");
    assert!(loc.contains("channel=@wireguard"), "{loc}");
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@wireguard")
        .expect("public add stores the @handle, not the numeric id");
    assert_eq!(row.channel, "@wireguard");
    let resp = app
        .clone()
        .oneshot(authed_req("/channels", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("@wireguard"), "{body}");
    assert!(body.contains(">public<"), "{body}");
    // Private refuses a non-ID: fixed copy, nothing stored.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=private&channel=@nope",
            &token,
        ))
        .await
        .unwrap();
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("error=private-id"), "{loc}");
    assert!(!db.list_channels(None).iter().any(|c| c.channel == "@nope"));
    // The refusal banner renders on the ADD page (CHANNELS_MASTER_DETAIL_SPEC
    // §6: shape/empty error PRGs retargeted to /channels/add?error=…).
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/add?error=private-id", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Private channels are addressed by their numeric -100 ID only — paste the ID, or switch Visibility to Public if the channel has an @handle."),
        "{body}"
    );
    // Public refuses a numeric paste (a private ID): the copy points at the
    // visibility switch.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=public&channel=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("error=public-shape"), "{loc}");
    assert!(
        !db.list_channels(None)
            .iter()
            .any(|c| c.channel == "-1001234567890123"),
        "the refused paste stores nothing"
    );
}

/// §3.4.5/§8.2.2: a typed Title wins over getChat and is saved via
/// set_channel_title — the redirect carries no notitle even though getChat
/// is unreachable, and the row renders as "Title (id)".
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_user_title_wins_and_is_stored() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=public&channel=@foo&title=Wire+Guard",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(
        loc.contains("added=1") && loc.contains("channel=@foo"),
        "{loc}"
    );
    assert!(
        !loc.contains("notitle"),
        "a typed title means nothing is unresolved: {loc}"
    );
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@foo")
        .expect("row added");
    assert_eq!(row.title.as_deref(), Some("Wire Guard"));
    let resp = app.oneshot(authed_req("/channels", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Wire Guard (@foo)"), "{body}");
}

/// §3.4.5/§8.2.3: notitle rides the redirect only when neither the user
/// typed a title nor getChat answered.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_notitle_only_when_no_title_at_all() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    // Private numeric ID, no title: the honest notitle flag rides along.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=private&channel=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(loc.contains("notitle=1"), "{loc}");
    assert!(loc.contains("channel=-1001234567890123"), "{loc}");
    // A typed title: no notitle.
    let resp = app
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=private&channel=-1001234567890124&title=Ledger",
            &token,
        ))
        .await
        .unwrap();
    let loc = resp.headers().get("location").unwrap();
    let loc = loc.to_str().unwrap();
    assert!(!loc.contains("notitle"), "{loc}");
}

/// §4/§8.2.4: the post-add reveal — an anchored add highlights and focuses
/// the new row and the in-panel callout owns the message (top banner
/// suppressed); legacy redirects without the anchor keep today's banners.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_added_reveal_suppresses_the_top_banner() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@foo", 0).unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(authed_req("/channels?added=1&channel=@foo", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("row-added"), "{body}");
    assert!(body.contains(r#"tabindex="-1""#), "{body}");
    assert!(body.contains("data-focus-on-load"), "{body}");
    assert!(body.contains("is in."), "{body}");
    assert!(
        !body.contains("Channel added."),
        "one outcome, one banner — the callout owns it: {body}"
    );
    // Legacy redirect without the anchor keeps today's banner.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels?added=1", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Channel added."), "{body}");
    assert!(!body.contains("row-added"), "{body}");
    // The notitle callout variant replaces the top notitle banner.
    let resp = app
        .oneshot(authed_req(
            "/channels?added=1&notitle=1&channel=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("I couldn't fetch its name from Telegram."),
        "{body}"
    );
    assert!(
        !body.contains("Channel added — I couldn't fetch its name from Telegram"),
        "{body}"
    );
}

/// §5a/§8.2.5: POST /channels/name saves the display label only — owner-only,
/// PRG ?named=1, the "Channel name saved." banner, and the updated row label.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_name_saves_label_and_is_owner_only() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.add_channel("@one", 0).unwrap();
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let app = router(state);
    // A seller session is refused with 403 (same shape as the default test).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/name",
            "channel=@one&name=X",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(db.list_channels(None)[0].title, None, "nothing stored");
    // The owner saves the label; the identifier never moves.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/name",
            "channel=@one&name=Tech+Tips",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels/manage?channel=@one&named=1"
    );
    let row = db
        .list_channels(None)
        .into_iter()
        .find(|c| c.channel == "@one")
        .unwrap();
    assert_eq!(row.channel, "@one", "Edit name never retargets");
    assert_eq!(row.title.as_deref(), Some("Tech Tips"));
    // The banner and the updated label surface on the detail view
    // (CHANNELS_MASTER_DETAIL_SPEC §6).
    let resp = app
        .oneshot(authed_req("/channels/manage?channel=@one&named=1", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Channel name saved."), "{body}");
    assert!(body.contains("Tech Tips (@one)"), "{body}");
}

/// §5b/§8.2.6: Change address is a collapsed danger control with the
/// shape-branched warning and the Retarget confirm; the unchanged submit
/// still hits the NOCASE guard (renamed=1&same=1) and writes nothing.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_change_address_control_and_guard() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/add",
            "visibility=private&channel=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    // The collapsed danger control lives on the detail view
    // (CHANNELS_MASTER_DETAIL_SPEC §5.4) with the private-row warning.
    let resp = app
        .clone()
        .oneshot(authed_req(
            "/channels/manage?channel=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Change address…"), "{body}");
    assert!(
        body.contains(
            "Private channels must keep their -100 ID. Changing this retargets where posts go."
        ),
        "{body}"
    );
    assert!(body.contains(">Retarget</button>"), "{body}");
    assert!(body.contains(r#"action="/channels/rename""#), "{body}");
    // An unchanged submit (the box pre-fills the current address) hits the
    // guard: nothing is rewritten and the honest banner replaces "renamed" —
    // on the detail view.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/rename",
            "channel=-1001234567890123&new=-1001234567890123",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels/manage?channel=-1001234567890123&renamed=1&same=1"
    );
    let resp = app
        .oneshot(authed_req(
            "/channels/manage?channel=-1001234567890123&renamed=1&same=1",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Nothing changed — that's still the channel's address."),
        "{body}"
    );
}

/// CHANNELS_MASTER_DETAIL_SPEC §10.4.1: the dedicated add page — owner and
/// seller variants render 200 with their own form; unauthenticated → /login;
/// admin → 403 (mirroring the POST gate).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_add_page_roles_and_variants() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = state
        .db
        .get_web_account_by_username("admin")
        .expect("admin account exists");
    let admin = state.db.create_web_session(acc_id, "admin", None, 7);
    let app = router(state);
    // Owner variant: the full form (Title + visibility + identifier) and the
    // back link.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/add", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"<input id="ch-title" name="title""#),
        "{body}"
    );
    assert!(body.contains(r#"<fieldset class="visibility">"#), "{body}");
    assert!(
        body.contains("<button type=\"submit\">Add channel</button>"),
        "{body}"
    );
    assert!(body.contains(">Back to channels</a>"), "{body}");
    // Seller variant: `Set channel` submit, NO Title field.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/add", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("<button type=\"submit\">Set channel</button>"),
        "{body}"
    );
    assert!(
        !body.contains(r#"<input id="ch-title" name="title""#),
        "seller add must not offer a Title control: {body}"
    );
    // Unauthenticated → /login; admin → 403.
    let resp = app.clone().oneshot(get_req("/channels/add")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
    let resp = app
        .oneshot(authed_req("/channels/add", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// CHANNELS_MASTER_DETAIL_SPEC §10.4.2: the master list is read-first —
/// label + state badges + exactly one `Manage →` affordance per row, no
/// inline edit controls, and the page-head `Add channel` primary action.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_master_list_is_read_first() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@foo", 0).unwrap();
    state.db.set_channel_title("@foo", Some("Wire Guard"));
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/channels", &token)).await.unwrap();
    let body = body_string(resp).await;
    // The one affordance, named per channel (§4.2 accessible-name contract).
    assert!(body.contains(r#"aria-label="Manage Wire Guard""#), "{body}");
    assert!(
        body.contains(">Manage <span aria-hidden=\"true\">→</span></a>"),
        "{body}"
    );
    // The page-head primary action points at the dedicated add page.
    assert!(
        body.contains(r#"href="/channels/add">Add channel</a>"#),
        "{body}"
    );
    // No inline edit controls remain in rows.
    assert!(!body.contains("Edit name"), "{body}");
    assert!(!body.contains("Change address…"), "{body}");
    assert!(
        !body.contains("inline-form"),
        "zero inline forms on the master list: {body}"
    );
}

/// CHANNELS_MASTER_DETAIL_SPEC §10.4.3: the manage happy path — the detail
/// view renders the five sections with their forms (Identity → Address →
/// Default → Posting as → Danger zone).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_manage_happy_path_renders_all_sections() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@one", 0).unwrap();
    state.db.set_channel_title("@one", Some("Tech Tips"));
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(authed_req("/channels/manage?channel=@one", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(">Back to channels</a>"), "{body}");
    assert!(body.contains("Tech Tips (@one)"), "{body}");
    assert!(body.contains(r#"action="/channels/name""#), "{body}");
    assert!(body.contains(">Save name</button>"), "{body}");
    assert!(body.contains(">Set default</button>"), "{body}");
    assert!(body.contains(">Post as user…</button>"), "{body}");
    assert!(body.contains(r#"action="/channels/remove""#), "{body}");
    assert!(body.contains("Danger zone"), "{body}");
}

/// CHANNELS_MASTER_DETAIL_SPEC §10.4.4: manage fails closed — unknown rows
/// and foreign-bot scoping render the scoped refusal (owner), cross-scope
/// sellers are refused honestly, and the seller's own channel renders facts
/// + Delete without the owner-only controls.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_manage_refusals_fail_closed() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.add_channel("@one", 0).unwrap();
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    db.set_seller_channel(42, "@mine");
    let app = router(state);
    // Unknown X (owner): the honest global-list refusal.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@ghost", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("That channel isn't in the global list"),
        "{body}"
    );
    // Missing ?channel= → back to the list.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/channels");
    // Bot-scoped owner + a row that resolves to another bot (same setup as
    // the list's scoped-empty test: no tokens, so every row resolves to the
    // placeholder bot, never to the created one).
    let bot_id = db.create_bot("secondary", 1, None, "");
    db.set_web_session_selected_bot(&sha256_hex(&owner), Some(bot_id));
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@one", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("belongs to another bot"), "{body}");
    // Seller + foreign X → fail closed.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@one", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("isn't yours to manage"), "{body}");
    // Seller + own X: facts + Danger-zone Delete, no owner controls.
    let resp = app
        .clone()
        .oneshot(authed_req("/channels/manage?channel=@mine", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("bot-managed"), "{body}");
    assert!(body.contains(r#"action="/channels/remove""#), "{body}");
    assert!(!body.contains("Set default"), "{body}");
    assert!(!body.contains("Post as"), "{body}");
    assert!(!body.contains(r#"action="/channels/name""#), "{body}");
}

/// CHANNELS_MASTER_DETAIL_SPEC §10.4.5: the detail view surfaces its own
/// action flags (the §6 matrix), and legacy `/channels?flag=1` bookmarks
/// keep rendering their banners (flag-keyed, page-agnostic tolerance).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn channels_manage_flash_matrix_and_legacy_tolerance() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@one", 0).unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    // Both detail flags on one GET: the matrix renders both banners.
    let resp = app
        .clone()
        .oneshot(authed_req(
            "/channels/manage?channel=@one&default=1&named=1",
            &token,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Default channel set."), "{body}");
    assert!(body.contains("Channel name saved."), "{body}");
    // Legacy list-level bookmark keeps its banner.
    let resp = app
        .oneshot(authed_req("/channels?renamed=1&same=1", &token))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Nothing changed — that's still the channel's address."),
        "{body}"
    );
}
