use super::*;
// ---------- Channel message ops ----------

#[derive(Deserialize)]
pub(super) struct PostScheduleForm {
    pub(super) chat_id: String,
    pub(super) content: String,
    pub(super) scheduled_at: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn post_schedule(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostScheduleForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/posts")).into_response(),
        }
    };
    if f.chat_id.trim().is_empty() || f.content.trim().is_empty() {
        return Redirect::to(&state.url("/posts")).into_response();
    }
    // The destination is client-supplied and the scheduler sends through the
    // bot bound to it, so it is authorized before the row is written (H1).
    if !may_mutate_channel(&state, &session, &f.chat_id) {
        return forbidden_page(&state, "Posts", "Posts");
    }
    state
        .db
        .create_scheduled_post(seller_id, &f.chat_id, &f.content, "text", &f.scheduled_at);
    Redirect::to(&state.url("/posts")).into_response()
}

#[derive(Deserialize)]
pub(super) struct PostEditForm {
    pub(super) chat_id: String,
    pub(super) message_id: String,
    pub(super) text: String,
    pub(super) post_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct PostIdForm {
    pub(super) chat_id: String,
    pub(super) message_id: String,
    pub(super) post_id: Option<String>,
}

/// Shared body of the channel-addressed post mutations (edit/delete/pin/
/// unpin): the channel target is authorized first (review H1), then the
/// post row (a seller touches only their own rows; missing = denial), then
/// the DB action and the PRG back to /posts.
async fn run_post_row_op<T, F, Fut, D>(
    state: &WebState,
    session: &Session,
    chat_id: &str,
    message_id: &str,
    post_id: Option<&str>,
    op: F,
    db_action: D,
) -> Response
where
    F: FnOnce(teloxide::Bot, teloxide::types::Recipient, i32) -> Fut,
    Fut: std::future::IntoFuture<Output = Result<T, teloxide::RequestError>>,
    D: FnOnce(&Db, Option<i64>),
{
    if !may_mutate_channel(state, session, chat_id) {
        return forbidden_page(state, "Posts", "Posts");
    }
    let pid = post_id.and_then(|s| s.trim().parse::<i64>().ok());
    if !may_mutate_post(state, session, pid) {
        return forbidden_page(state, "Posts", "Posts");
    }
    let msg_id: i32 = message_id.trim().parse().unwrap_or(0);
    if msg_id != 0 {
        let ctx = state.bots.for_channel(chat_id);
        let _ = op(ctx.bot.clone(), crate::util::to_chat_id(chat_id), msg_id).await;
    }
    db_action(&state.db, pid);
    Redirect::to(&state.url("/posts")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn post_edit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostEditForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_post_row_op(
        &state,
        &session,
        &f.chat_id,
        &f.message_id,
        f.post_id.as_deref(),
        |bot, chat, id| bot.edit_message_text(chat, teloxide::types::MessageId(id), &f.text),
        |db, pid| {
            if let Some(pid) = pid {
                let _ = db.update_post_content(pid, &f.text);
            }
        },
    )
    .await
}

#[tracing::instrument(skip_all)]
pub(super) async fn post_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_post_row_op(
        &state,
        &session,
        &f.chat_id,
        &f.message_id,
        f.post_id.as_deref(),
        |bot, chat, id| bot.delete_message(chat, teloxide::types::MessageId(id)),
        |db, pid| {
            if let Some(pid) = pid {
                let _ = db.delete_post(pid);
            }
        },
    )
    .await
}

#[tracing::instrument(skip_all)]
pub(super) async fn post_pin(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_post_row_op(
        &state,
        &session,
        &f.chat_id,
        &f.message_id,
        f.post_id.as_deref(),
        |bot, chat, id| bot.pin_chat_message(chat, teloxide::types::MessageId(id)),
        |_, _| {},
    )
    .await
}

#[tracing::instrument(skip_all)]
pub(super) async fn post_unpin(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_post_row_op(
        &state,
        &session,
        &f.chat_id,
        &f.message_id,
        f.post_id.as_deref(),
        |bot, chat, id| {
            bot.unpin_chat_message(chat)
                .message_id(teloxide::types::MessageId(id))
        },
        |_, _| {},
    )
    .await
}

#[derive(Deserialize)]
pub(super) struct ChannelInviteForm {
    pub(super) chat_id: String,
}

/// Owner-only. The link is rendered inline — shown exactly once, never
/// stored and never logged; failures re-render the channels page banner.
#[tracing::instrument(skip_all)]
pub(super) async fn channel_invite(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelInviteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Channels", "Invite links");
    }
    let chat_id = f.chat_id.trim().to_string();
    match effective_bot(&state, &session)
        .bot
        .create_chat_invite_link(chat_id.clone())
        .await
    {
        Ok(r) => {
            let body = format!(
                r#"<h1>Invite link <span class="sub">{}</span></h1>
<div class="alert alert-success" role="status" aria-live="polite">Invite link created for <span class="mono">{}</span>. Copy it now — it is shown once and never stored or logged.</div>
<article class="panel"><p><code class="copyable">{}</code></p>
<p><a href="{}">Back to channels</a></p></article>"#,
                esc(&chat_id),
                esc(&chat_id),
                esc(&r.invite_link),
                state.url("/channels")
            );
            Html(html_page(&state, Some(&session), "Invite link", &body)).into_response()
        }
        Err(e) => channels_error(
            &state,
            &session,
            &format!("Could not create the invite link: {}", esc(&e.to_string())),
        ),
    }
}

#[derive(Deserialize)]
pub(super) struct ChannelTitleForm {
    pub(super) chat_id: String,
    pub(super) title: String,
}

/// Shared body of the channel title/description mutations (H1 gate + PRG).
async fn run_channel_meta_op<F, Fut>(
    state: &WebState,
    session: &Session,
    chat_id: &str,
    op: F,
) -> Response
where
    F: FnOnce(teloxide::Bot, teloxide::types::Recipient) -> Fut,
    Fut: std::future::IntoFuture<Output = Result<teloxide::types::True, teloxide::RequestError>>,
{
    if !may_mutate_channel(state, session, chat_id) {
        return forbidden_page(state, "Channels", "Channels");
    }
    let ctx = state.bots.for_channel(chat_id);
    let _ = op(ctx.bot.clone(), crate::util::to_chat_id(chat_id)).await;
    Redirect::to(&state.url("/channels")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn channel_title(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelTitleForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_channel_meta_op(&state, &session, &f.chat_id, |bot, chat| {
        bot.set_chat_title(chat, &f.title)
    })
    .await
}

#[derive(Deserialize)]
pub(super) struct ChannelDescForm {
    pub(super) chat_id: String,
    pub(super) description: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn channel_desc(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelDescForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    run_channel_meta_op(&state, &session, &f.chat_id, |bot, chat| {
        bot.set_chat_description(chat).description(&f.description)
    })
    .await
}

#[derive(Deserialize)]
pub(super) struct SetupForm {
    pub(super) bot_token: String,
    pub(super) owner_id: String,
    pub(super) default_channel: Option<String>,
    pub(super) crypto_token: Option<String>,
    pub(super) web_username: Option<String>,
    pub(super) web_password: Option<String>,
}

/// Non-secret values preserved across an error re-render (F1.1/R5); secrets
/// (token, Crypto Pay token, password) always re-render empty.
struct SetupPreserved {
    owner_id: String,
    default_channel: String,
    web_username: String,
}

impl SetupPreserved {
    fn fresh() -> Self {
        Self {
            owner_id: String::new(),
            default_channel: String::new(),
            web_username: "owner".to_string(),
        }
    }
}

/// The BotFather recipe (F1.3, verbatim); duplicated in settings.rs (F2.2).
const BOTFATHER_RECIPE: &str = r#"<details open><summary>How to get a bot token (2 minutes, no skills needed)</summary><ol><li>Open Telegram and message <a href="https://t.me/BotFather" rel="noopener">@BotFather</a>.</li><li>Send <code>/newbot</code>, choose a display name, then a username ending in <code>bot</code>.</li><li>Copy the token it replies with (<code>123456:ABC-…</code>) and paste it below.</li><li>For your owner ID, message <a href="https://t.me/userinfobot" rel="noopener">@userinfobot</a> — it replies with your numeric ID.</li><li>Keep the bot's privacy mode ON; add it as <b>admin</b> to your channel when you sell access.</li></ol></details>"#;

/// The setup page body as one builder (F1.3): step line, two h2 groups,
/// collapsed optional disclosure, explicit for/id pairs (§3.1); `alert`
/// renders above the recipe so errors announce first (§3.4).
fn setup_form_body(state: &WebState, alert: Option<&str>, preserved: &SetupPreserved) -> String {
    format!(
        r#"<h1>Welcome — first-run setup</h1>
<p class="help">Two steps. This page is step 1 — step 2 is restarting the app.</p>
<p class="help">Connect your bot and claim the console. Takes about two minutes; every field has a hint below.</p>
{alert}
{recipe}
<form method="post" action="{setup_url}">
<h2>Your bot</h2>
<label for="setup-bot-token">Bot token</label>
<span class="secret-field"><input type="password" id="setup-bot-token" name="bot_token" required autocomplete="off" placeholder="123456:ABC-DEF…">{toggle}</span><span class="help">From @BotFather. Stored privately on this machine (.env) and never shown to anyone.</span>
<label for="setup-owner-id">Owner Telegram user ID</label>
<input type="text" id="setup-owner-id" name="owner_id" required inputmode="numeric" placeholder="123456789" value="{owner_id}">
<span class="help">Your own Telegram account number — message @userinfobot and it replies with it. This account controls the whole console.</span>
<h2>Your console login</h2>
<div class="form-row">
<div><label for="setup-web-username">Dashboard username</label><input type="text" id="setup-web-username" name="web_username" value="{web_username}" autocomplete="username"></div>
<div><label for="setup-web-password">Dashboard password</label>
<span class="secret-field"><input type="password" id="setup-web-password" name="web_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters — this unlocks the console.</span></div>
</div>
<details><summary>Optional — add later in Settings</summary>
<label for="setup-default-channel">Default channel</label>
<input type="text" id="setup-default-channel" name="default_channel" placeholder="@yourchannel" value="{default_channel}">
<span class="help">Where posts and paid access go when nothing else is set, for example @myshopnews.</span>
<span class="help">You can add this any time in Settings → Bot connection.</span>
<label for="setup-crypto-token">Crypto Pay token</label>
<span class="secret-field"><input type="password" id="setup-crypto-token" name="crypto_token" autocomplete="off" placeholder="from @CryptoBot → Crypto Pay → Create App">{toggle}</span><span class="help">From @CryptoBot → Crypto Pay → Create App. Lets the console verify payments automatically.</span>
<span class="help">You can add this any time in Settings → Bot connection.</span>
</details>
<div class="form-row">
<div><button type="submit">Save and continue</button></div>
<div><button type="submit" class="btn btn-secondary" formaction="{skip_url}" formnovalidate onclick="return confirm('Finish setup later in Settings → Bot connection? This page won\'t come back.')">Skip for now</button></div>
</div>
<p class="help">You can finish setup later in Settings → Bot connection. This page won't come back.</p>
</form>"#,
        setup_url = state.url("/setup"),
        skip_url = state.url("/setup/skip"),
        recipe = BOTFATHER_RECIPE,
        alert = alert.unwrap_or_default(),
        owner_id = esc(&preserved.owner_id),
        default_channel = esc(&preserved.default_channel),
        web_username = esc(&preserved.web_username),
        toggle = SECRET_TOGGLE,
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn setup_page(State(state): State<WebState>) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    let body = setup_form_body(&state, None, &SetupPreserved::fresh());
    Html(html_page_bare(&state.base_path, "Setup", &body)).into_response()
}

/// "Skip for now": close the first-run gate WITHOUT creating an account or
/// writing .env; the button's confirm + help line (F1.5) are the warning.
#[tracing::instrument(skip_all)]
pub(super) async fn setup_skip(State(state): State<WebState>) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    state.db.set_config("setup_done", "1");
    Redirect::to(&state.url("/login")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn setup_submit(
    State(state): State<WebState>,
    Form(f): Form<SetupForm>,
) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    let token = f.bot_token.trim().to_string();
    let uname = f
        .web_username
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "owner".to_string());
    let preserved = SetupPreserved {
        owner_id: f.owner_id.trim().to_string(),
        default_channel: f.default_channel.clone().unwrap_or_default(),
        web_username: uname.clone(),
    };
    // Every failure re-renders the full page in place with a focused alert —
    // never a "Back to setup" link that discards the form (F1.1).
    let render = |alert: String| -> Response {
        Html(html_page_bare(
            &state.base_path,
            "Setup",
            &setup_form_body(&state, Some(&alert), &preserved),
        ))
        .into_response()
    };
    // Local validation FIRST (F1.2): all problems in one alert, Telegram
    // is not contacted until the form is locally sane.
    let mut problems: Vec<&str> = Vec::new();
    if token.is_empty() {
        problems.push("Paste the bot token from @BotFather — it looks like 123456:ABC-DEF….");
    }
    let owner_id = f.owner_id.trim().parse::<i64>().ok().filter(|v| *v > 0);
    if owner_id.is_none() {
        problems
            .push("Owner ID must be the number @userinfobot replies with — for example 123456789.");
    }
    let pw = f.web_password.clone().unwrap_or_default();
    if pw.len() < 6 {
        problems.push("Choose a dashboard password of at least 6 characters.");
    }
    if !problems.is_empty() {
        return render(focused_error_alert(&format!(
            "{}<br>Nothing was saved.",
            problems.join("<br>")
        )));
    }
    if let Err(e) = validate_token_username(&token).await {
        return render(focused_error_alert(&token_error_copy(&e)));
    }
    let channel = f.default_channel.clone().filter(|s| !s.trim().is_empty());
    let crypto = f.crypto_token.clone().filter(|s| !s.trim().is_empty());
    if let Err(msg) = persist_setup(
        &state,
        &token,
        owner_id.unwrap_or(0),
        &uname,
        &pw,
        channel.as_deref(),
        crypto.as_deref(),
    )
    .await
    {
        return render(focused_error_alert(&msg));
    }
    // F1.6: what works now (the login) vs. what needs the restart (the bot),
    // with the same per-platform commands as restart_required_banner.
    let body = format!(
        r#"<div class="alert alert-success" role="status" aria-live="polite">Setup complete — your login works right now.</div>
<p>The bot itself starts after one restart. The running app keeps the settings it started with.</p>
<p class="help">Desktop: quit and reopen. Server (systemd): <code>systemctl restart telegram-agent</code>. Docker: <code>docker compose restart</code>.</p>
<p class="help">After restarting, sign in and follow the dashboard checklist: add a plan, then share your bot with customers.</p>
<p><a class="btn" href="{}">Go to sign in</a></p>"#,
        state.url("/login")
    );
    Html(html_page_bare(&state.base_path, "Setup", &body)).into_response()
}

/// Persist a locally-valid, Telegram-validated setup: write .env through
/// the shared key-preserving writer (this form's keys only; blank
/// optionals written empty so a re-run is idempotent), seed the owner
/// account, close the gate.
async fn persist_setup(
    state: &WebState,
    token: &str,
    owner_id: i64,
    uname: &str,
    pw: &str,
    channel: Option<&str>,
    crypto: Option<&str>,
) -> Result<(), String> {
    let kv = |k: &str, v: String| (k.to_string(), v);
    let updates = [
        kv("TELEGRAM_BOT_TOKEN", token.to_string()),
        kv("OWNER_ID", owner_id.to_string()),
        kv("DATABASE_URL", format!("sqlite:{}", state.cfg.db_path)),
        kv("DEFAULT_CHANNEL", channel.unwrap_or_default().to_string()),
        kv("CRYPTO_PAY_TOKEN", crypto.unwrap_or_default().to_string()),
        kv("WEB_USERNAME", uname.to_string()),
        kv("WEB_PASSWORD", pw.to_string()),
        kv("WEB_PORT", state.cfg.web_port().to_string()),
        kv("RUST_LOG", "info".to_string()),
    ];
    crate::config::write_env_file(&updates).map_err(|e| {
        format!(
            "Failed to write .env: {}. Nothing was saved.",
            esc(&e.to_string())
        )
    })?;
    if state.db.get_web_account_by_username(uname).is_none() {
        let _ = state.db.create_web_account(uname, pw, "owner", None).await;
    }
    if let Some(c) = channel {
        state.db.set_config("channel", c);
    }
    state.db.set_config("setup_done", "1");
    state.db.set_config("restart_required", "1");
    Ok(())
}
