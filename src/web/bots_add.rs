use super::*;
// ---------- Bot add wizard + bot-token validation (moved verbatim from
// bots_admin.rs, MANAGEMENT_DISCLOSURE_SPEC §1; no logic changes) ----------
use super::bots_admin::BotsQuery; // shared page query: /bots and /bots/add

#[derive(Deserialize, Default)]
pub(super) struct BotValidateForm {
    pub(super) token: String,
}

#[derive(Deserialize, Default)]
pub(super) struct BotAddForm {
    pub(super) key: String,
    pub(super) name: Option<String>,
    pub(super) username: Option<String>,
    pub(super) owner_user_id: String,
    pub(super) default_channel: Option<String>,
    pub(super) default_channel_id: Option<String>,
    pub(super) token_env: String,
}

// ---------- Bot config wizard (architecture) ----------

static LAST_BOT_SNIPPET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn is_slug_key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_token_env_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .map(|c| c == '_' || c.is_ascii_uppercase())
            .unwrap_or(false)
        && s.chars()
            .all(|c| c == '_' || c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn default_channel_value(f: &BotAddForm) -> Option<String> {
    f.default_channel
        .clone()
        .or_else(|| f.default_channel_id.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn validate_default_channel(v: &str) -> bool {
    if let Some(stripped) = v.strip_prefix('@') {
        !stripped.is_empty()
            && stripped
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    } else {
        v.parse::<i64>().is_ok()
    }
}

/// Why a token failed validation: the console could not reach Telegram at
/// all (Network) versus Telegram answered and refused the token (Rejected).
/// The two need different, honest copy (F1.2/F2.5).
pub(super) enum TokenValidationError {
    Network,
    Rejected(String),
}

/// The §R5 copy for a token-validation failure, shared by every surface
/// that checks a token (setup F1.2, settings F2.5): the honest verdict plus
/// the concrete next step, nothing saved either way.
pub(super) fn token_error_copy(e: &TokenValidationError) -> String {
    match e {
        TokenValidationError::Network => "Couldn't reach Telegram to check the token. Check your connection and try again. Nothing was saved.".to_string(),
        TokenValidationError::Rejected(_) => "Telegram rejected that bot token. Compare it against the message @BotFather sent you — it looks like 123456:ABC-DEF…. Nothing was saved.".to_string(),
    }
}

pub(super) async fn validate_token_username(token: &str) -> Result<String, TokenValidationError> {
    let url = format!("https://api.telegram.org/bot{}/getMe", token);
    let resp = match reqwest::Client::new().get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let kind = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connect"
            } else {
                "request"
            };
            // Never log the token or the full URL (which contains it).
            tracing::warn!(error_kind = kind, "bot token validation request failed");
            return Err(TokenValidationError::Network);
        }
    };
    let status = resp.status();
    let v: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => {
            tracing::warn!("bot token validation: invalid JSON response");
            return Err(TokenValidationError::Rejected(format!(
                "invalid response from Telegram (HTTP {})",
                status
            )));
        }
    };
    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let reason = v
            .get("description")
            .and_then(|d| d.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Telegram rejected the token (HTTP {})", status));
        // `reason` comes from Telegram's API and never contains the token.
        tracing::warn!(reason = %reason, "bot token validation failed");
        return Err(TokenValidationError::Rejected(reason));
    }
    match v
        .get("result")
        .and_then(|r| r.get("username"))
        .and_then(|u| u.as_str())
    {
        Some(u) => Ok(u.to_string()),
        None => {
            tracing::warn!("bot token validation: response missing username");
            Err(TokenValidationError::Rejected(
                "Telegram response did not include a bot username".to_string(),
            ))
        }
    }
}

/// The duplicate-nickname rejection (§R5 shape), shared by both duplicate
/// checks in the add path.
fn duplicate_key_msg(key: &str) -> String {
    format!(
        "A bot with the nickname {} is already in your config file. Pick a different nickname. Nothing was saved.",
        esc(key)
    )
}

/// Non-secret Step-2 values, preserved across error re-renders (R5: never
/// discard a form). Secrets never ride here — the wizard has none in Step 2.
struct BotAddPreserved {
    key: String,
    name: String,
    username: String,
    owner_user_id: String,
    default_channel: String,
    token_env: String,
}

impl BotAddPreserved {
    fn from_form(f: &BotAddForm) -> Self {
        BotAddPreserved {
            key: f.key.trim().to_string(),
            name: f.name.clone().unwrap_or_default(),
            username: f.username.clone().unwrap_or_default(),
            owner_user_id: f.owner_user_id.trim().to_string(),
            default_channel: default_channel_value(f).unwrap_or_default(),
            token_env: f.token_env.trim().to_string(),
        }
    }

    /// Pre-fill from a freshly validated token: the username becomes the
    /// suggestion for nickname/name/@username, and the token-env name is
    /// derived from the nickname (F3.1/F3.2).
    fn from_validated_username(u: &str) -> Self {
        let key: String = u
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                    c
                } else if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .take(32)
            .collect();
        let suggested_env = suggested_token_env(&key);
        BotAddPreserved {
            key,
            name: u.to_string(),
            username: format!("@{}", u),
            owner_user_id: String::new(),
            default_channel: String::new(),
            token_env: suggested_env,
        }
    }
}

/// `BOT_` + the nickname uppercased with `-`/`_` folded to `_` (F3.2).
fn suggested_token_env(key: &str) -> String {
    format!("BOT_{}", key.replace(['-', '_'], "_").to_ascii_uppercase())
}

/// Local validation of the Step-2 form: every check the handler can make
/// without touching disk or the network, with one error message per failure.
struct BotAddValidated {
    key: String,
    name: String,
    owner: i64,
    token_env: String,
    dc: Option<String>,
}

fn validate_bot_add(f: &BotAddForm) -> Result<BotAddValidated, String> {
    let key = f.key.trim().to_string();
    if !is_slug_key(&key) {
        return Err(
            "The bot nickname must be 1-32 lowercase letters, numbers, or dashes — for example main. Nothing was saved."
                .to_string(),
        );
    }
    let name = f
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| f.username.clone().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| key.clone());
    let name = name.trim().to_string();
    if name.is_empty() || name.len() > 64 {
        return Err("The name must be 1-64 characters. Nothing was saved.".to_string());
    }
    let owner: i64 = match f.owner_user_id.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return Err(
                "Owner user ID must be the number @userinfobot replies with — for example 123456789. Nothing was saved."
                    .to_string(),
            );
        }
    };
    let token_env = f.token_env.trim().to_string();
    if !is_token_env_name(&token_env) {
        return Err(
            "The token setting name must be uppercase letters, numbers, and underscores, starting with a letter — for example BOT_MAIN. Nothing was saved."
                .to_string(),
        );
    }
    let dc = default_channel_value(f);
    if let Some(v) = &dc
        && !validate_default_channel(v)
    {
        return Err(
            "Default channel must be an @channelname or a numeric chat ID — for example @myshopnews. Nothing was saved."
                .to_string(),
        );
    }
    Ok(BotAddValidated {
        key,
        name,
        owner,
        token_env,
        dc,
    })
}

/// Re-render the add page after a Step-2 validation failure: focused alert
/// on top, Step 2 still open with everything the user typed preserved (R5).
fn bot_add_error_page(state: &WebState, session: &Session, f: &BotAddForm, msg: &str) -> Response {
    let preserved = BotAddPreserved::from_form(f);
    Html(render_add_page(
        state,
        session,
        &focused_error_alert(msg),
        "",
        Some(&preserved),
    ))
    .into_response()
}

/// The copy-paste fallback panel (F3.3): the generated `[[bots]]` block with
/// two-step manual instructions, replacing the old dead-end file download.
fn bot_snippet_panel(state: &WebState, toml_block: &str) -> String {
    format!(
        r#"<div class="alert alert-warn" role="status" aria-live="polite">This bot couldn't be added automatically — the configured bot config file is missing, unreadable, or unwritable. Two minutes, two steps:</div>
<p><b>1.</b> Copy the block below into your bot config file (the path in <code>BOTS_CONFIG</code>), creating it if it doesn't exist.</p>
<code class="copyable">{toml}</code>
<p><b>2.</b> Make sure <code>BOTS_CONFIG=/path/to/bots.toml</code> is set in <code>.env</code> and the file is writable, then restart the app. Your bot starts polling after the restart. Restart: desktop quit &amp; reopen; server <code>systemctl restart daybook</code>; Docker <code>docker compose restart</code>.</p>
<p><a class="btn btn-secondary" href="{snippet_url}">Download this block as a file</a>
<a class="btn btn-secondary" href="{bots_url}">Back to Bots</a></p>"#,
        toml = esc(toml_block),
        snippet_url = state.url("/bots/add/snippet"),
        bots_url = state.url("/bots"),
    )
}

/// The Add-bot page. Step 2 renders only after a validated token (`step2`
/// carries the pre-filled/preserved values); otherwise it shows a locked
/// teaser so the two-step shape is visible without being usable (F3.1).
fn render_add_page(
    state: &WebState,
    session: &Session,
    banner: &str,
    snippet_panel: &str,
    step2: Option<&BotAddPreserved>,
) -> String {
    let step1 = format!(
        r#"<h2>Step 1 — Validate token</h2>
<p class="help">Step 1 of 2 — prove the token works</p>
<form method="post" action="{action}" class="inline-form">
  <label for="bot-add-token">Bot token</label>
  <span class="secret-field"><input id="bot-add-token" type="password" name="token" required autocomplete="new-password" placeholder="123456:ABC-DEF…">{toggle}</span>
  <button type="submit">Validate with Telegram</button>
</form>"#,
        action = state.url("/bots/add/validate"),
        // B16 F-2 rider (MANAGEMENT_DISCLOSURE_SPEC): the add wizard's token
        // input gets the same reveal toggle as every other secret field.
        toggle = SECRET_TOGGLE
    );
    let step2_html = match step2 {
        Some(p) => format!(
            r#"<h2>Step 2 — Add bot</h2>
<p class="help">Step 2 of 2 — describe the bot</p>
<form method="post" action="{action}">
  <label for="bot-add-key">Bot nickname</label>
  <input id="bot-add-key" name="key" pattern="[a-z0-9_-]{{1,32}}" required value="{key}">
  <span class="help">A short name for this bot, used only inside this console. Lowercase letters, numbers, dashes — for example main.</span>
  <label for="bot-add-username">Username</label>
  <input id="bot-add-username" name="username" value="{username}">
  <span class="help">Optional — the bot's @username on Telegram, filled in for you after validation.</span>
  <label for="bot-add-name">Name</label>
  <input id="bot-add-name" name="name" value="{name}">
  <span class="help">Optional — how this bot is named in lists here. Defaults to the nickname.</span>
  <label for="bot-add-owner">Owner user ID</label>
  <input id="bot-add-owner" type="number" name="owner_user_id" required value="{owner}">
  <span class="help">Your own Telegram account number (@userinfobot replies with it). This bot's customers and channels belong to this account.</span>
  <label for="bot-add-channel">Default channel</label>
  <input id="bot-add-channel" name="default_channel" value="{channel}" placeholder="@channel or -100…">
  <span class="help">Optional — where this bot posts and hands out paid access when nothing else is set. Either @channelname or the numeric chat ID.</span>
  <label for="bot-add-token-env">Where the token will be stored</label>
  <input id="bot-add-token-env" name="token_env" pattern="[A-Z_][A-Z0-9_]*" required value="{token_env}">
  <span class="help">The setting name in your config file that will hold this bot's token, for example BOT_TOKEN_MAIN. The console never stores the token itself — you paste the actual token into .env (or your bot config file) yourself.</span>
  <button type="submit">Add bot</button>
</form>"#,
            action = state.url("/bots/add"),
            key = esc(&p.key),
            username = esc(&p.username),
            name = esc(&p.name),
            owner = esc(&p.owner_user_id),
            channel = esc(&p.default_channel),
            token_env = esc(&p.token_env),
        ),
        None => r#"<h2>Step 2 — Add bot</h2>
<p class="help">Step 2 of 2 — describe the bot. It unlocks here once the token validates.</p>"#
            .to_string(),
    };
    let body = format!(
        r#"<h1>Add bot <span class="sub">config wizard</span></h1>
{banner}
{step1}
{snippet_panel}
{step2_html}
<p><a href="{snippet_link}">View last generated snippet</a></p>"#,
        banner = banner,
        step1 = step1,
        snippet_panel = snippet_panel,
        step2_html = step2_html,
        snippet_link = state.url("/bots/add/snippet")
    );
    html_page(state, Some(session), "Add bot", &body)
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_add_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BotsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Bots", "Add bot").into_response();
    }
    let mut banner = String::new();
    if let Some(k) = q.added.as_deref().filter(|s| !s.trim().is_empty()) {
        banner.push_str(&format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Bot <span class="mono">{}</span> added — the bot starts polling after the next restart.</div>"#,
            esc(k.trim())
        ));
    }
    let snippet_panel = if q.snippet.as_deref() == Some("1") {
        LAST_BOT_SNIPPET
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .map(|toml| bot_snippet_panel(&state, &toml))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let body = render_add_page(&state, &session, &banner, &snippet_panel, None);
    Html(body).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_add_validate(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotValidateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Bots", "Add bot").into_response();
    }
    let token = f.token.trim().to_string();
    if token.is_empty() {
        let body = render_add_page(
            &state,
            &session,
            &focused_error_alert(
                "Paste the bot token from @BotFather — it looks like 123456:ABC-DEF…. Nothing was saved.",
            ),
            "",
            None,
        );
        return Html(body).into_response();
    }
    match validate_token_username(&token).await {
        Ok(u) => {
            let preserved = BotAddPreserved::from_validated_username(&u);
            let body = render_add_page(
                &state,
                &session,
                &format!(
                    r#"<div class="alert alert-success" role="status" aria-live="polite">Token valid — this is <span class="mono">@{}</span>. Now give the bot its console details below.</div>"#,
                    esc(&u)
                ),
                "",
                Some(&preserved),
            );
            Html(body).into_response()
        }
        Err(e) => {
            // The add-wizard keeps Telegram's own reason visible (it is the
            // surface most likely to meet an expired/revoked token); setup
            // and settings use the fixed §R5 copy instead.
            let msg = match e {
                TokenValidationError::Network => {
                    "Couldn't reach Telegram to check the token. Check your connection and try again. Nothing was saved."
                        .to_string()
                }
                TokenValidationError::Rejected(reason) => format!(
                    "Telegram rejected that bot token: {}. Compare it against the message @BotFather sent you — it looks like 123456:ABC-DEF…. Nothing was saved.",
                    esc(&reason)
                ),
            };
            let body = render_add_page(&state, &session, &focused_error_alert(&msg), "", None);
            Html(body).into_response()
        }
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_add_submit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotAddForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Bots", "Add bot").into_response();
    }
    let v = match validate_bot_add(&f) {
        Ok(v) => v,
        Err(msg) => return bot_add_error_page(&state, &session, &f, &msg),
    };
    // Config-file state decides write vs snippet fallback. When BOTS_CONFIG is
    // unset (fresh install), auto-create a bots.toml next to the data dir and
    // persist BOTS_CONFIG into .env so the owner never has to hand-edit files.
    let mut cfgst = bot_config_state();
    let auto_created_config = if cfgst.path.is_none() {
        let default_path = crate::config::data_dir().join("bots.toml");
        let _ = crate::config::write_env_file_replacing(&[(
            "BOTS_CONFIG",
            Some(default_path.to_str().unwrap_or("")),
        )]);
        cfgst.path = Some(default_path);
        cfgst.force_fallback = false;
        cfgst.writable = cfgst
            .path
            .as_ref()
            .map(|p| {
                p.parent()
                    .map(|d| d.exists() || std::fs::create_dir_all(d).is_ok())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        cfgst.content = String::new();
        true
    } else {
        false
    };
    if duplicate_key_in(&cfgst.content, &v.key) {
        return bot_add_error_page(&state, &session, &f, &duplicate_key_msg(&v.key));
    }
    let toml_block = bot_toml_block(&v.key, &v.name, &v.token_env, v.owner, v.dc.as_deref());
    let can_write = !cfgst.force_fallback && cfgst.writable;
    let mut force_fallback = cfgst.force_fallback;
    let mut fresh_duplicate = false;
    let mut new_content = String::new();
    if can_write && let Some(p) = &cfgst.path {
        // Re-read content fresh so a concurrent edit cannot be clobbered.
        let content_now = if p.exists() {
            std::fs::read_to_string(p).unwrap_or_default()
        } else {
            String::new()
        };
        if !content_now.trim().is_empty() && parse_toml_document(&content_now).is_err() {
            force_fallback = true;
        } else if duplicate_key_in(&content_now, &v.key) {
            fresh_duplicate = true;
        } else {
            new_content = bot_config_new_content(&content_now, &toml_block);
        }
    }
    if fresh_duplicate {
        return bot_add_error_page(&state, &session, &f, &duplicate_key_msg(&v.key));
    }
    if can_write
        && !force_fallback
        && parse_toml_document(&new_content).is_ok()
        && let Some(p) = &cfgst.path
    {
        let wr = append_bot_block(p, &new_content);
        match wr {
            ConfigWriteResult::Written => {
                tracing::info!(key = %v.key, path = %p.display(), "bot added to config");
                // Read once at boot (R1): flag it like settings_save does.
                state.db.set_config("restart_required", "1");
                let loc = state.url(&format!("/bots/add?added={}", v.key));
                let restart_hint = if auto_created_config {
                    format!(
                        "A config file was created automatically at <code class=\"mono\">{}</code> and <code>BOTS_CONFIG</code> was saved to <code>.env</code>. ",
                        esc(&p.display().to_string())
                    )
                } else {
                    String::new()
                };
                let body = format!(
                    r#"<div class="alert alert-success" role="status" aria-live="polite">Bot <span class="mono">{}</span> added — the bot starts polling after the next restart. {}Don't forget to add <code class="mono">{}</code> (your bot token from @BotFather) to <code>.env</code> before restarting. Restart: desktop quit &amp; reopen; server <code>systemctl restart daybook</code>; Docker <code>docker compose restart</code>.</div>"#,
                    esc(&v.key),
                    restart_hint,
                    esc(&v.token_env)
                );
                return (
                    StatusCode::SEE_OTHER,
                    AppendHeaders([(header::LOCATION, loc)]),
                    Html(html_page(&state, Some(&session), "Add bot", &body)),
                )
                    .into_response();
            }
            ConfigWriteResult::BackupFailed => {
                return bot_add_error_page(
                    &state,
                    &session,
                    &f,
                    "Your bot config file could not be backed up, so nothing was written. Check the file's permissions and try again. Nothing was saved.",
                );
            }
            ConfigWriteResult::RetryAsFallback => {}
        }
    }

    // Fallback: hand the snippet back as a copy-paste panel (F3.3). Lock
    // failure degrades to no cached snippet — never panic in production.
    if let Ok(mut guard) = LAST_BOT_SNIPPET.lock() {
        *guard = Some(toml_block.clone());
    }
    let loc = state.url("/bots/add?snippet=1");
    (
        StatusCode::SEE_OTHER,
        AppendHeaders([(header::LOCATION, loc)]),
        Html(String::from("Redirecting to snippet…")),
    )
        .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_add_snippet(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Bots", "Add bot").into_response();
    }
    // Lock failure degrades to the empty placeholder — never a panic (P2-1).
    let snippet = LAST_BOT_SNIPPET
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| "# no snippet generated\n".to_string());
    (
        StatusCode::OK,
        AppendHeaders([
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"bot-snippet.toml\"".to_string(),
            ),
        ]),
        snippet,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step 2 pre-fills from the validated username (F3.1/F3.2): slug-safe
    /// nickname, name and @username carried over, token-env suggested.
    #[test]
    fn validated_username_prefills_step2() {
        let p = BotAddPreserved::from_validated_username("MyShop_Bot");
        assert_eq!(p.key, "myshop_bot");
        assert_eq!(p.name, "MyShop_Bot");
        assert_eq!(p.username, "@MyShop_Bot");
        assert_eq!(p.token_env, "BOT_MYSHOP_BOT");
        assert!(is_slug_key(&p.key));
        assert!(is_token_env_name(&p.token_env));
    }

    #[test]
    fn suggested_token_env_folds_separators() {
        assert_eq!(suggested_token_env("my-shop"), "BOT_MY_SHOP");
        assert_eq!(suggested_token_env("main"), "BOT_MAIN");
    }
}
