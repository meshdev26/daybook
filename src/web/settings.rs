use super::*;
// ---- Settings (owner-only, in-console configuration) ----

#[derive(Clone, Copy, Default)]
pub(super) struct SidebarPrefs {
    pub(super) ai: bool,
    pub(super) crypto: bool,
    pub(super) admins: bool,
    pub(super) sellers: bool,
    pub(super) webusers: bool,
}

impl SidebarPrefs {
    pub(super) fn from_json(s: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
        Self {
            ai: v.get("ai").and_then(|x| x.as_bool()).unwrap_or(false),
            crypto: v.get("crypto").and_then(|x| x.as_bool()).unwrap_or(false),
            admins: v.get("admins").and_then(|x| x.as_bool()).unwrap_or(false),
            sellers: v.get("sellers").and_then(|x| x.as_bool()).unwrap_or(false),
            webusers: v.get("webusers").and_then(|x| x.as_bool()).unwrap_or(false),
        }
    }
    pub(super) fn to_json(self) -> String {
        serde_json::json!({
            "ai": self.ai,
            "crypto": self.crypto,
            "admins": self.admins,
            "sellers": self.sellers,
            "webusers": self.webusers,
        })
        .to_string()
    }
}

pub(super) fn effective_sidebar_prefs(state: &WebState, session: &Session) -> SidebarPrefs {
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return SidebarPrefs::default();
    }
    let user_key = format!("sidebar_items:user:{}", session.account_id);
    if let Some(raw) = state.db.get_config(&user_key) {
        return SidebarPrefs::from_json(&raw);
    }
    if let Some(raw) = state.db.get_config("sidebar_items") {
        return SidebarPrefs::from_json(&raw);
    }
    SidebarPrefs {
        ai: state.cfg.ai_enabled,
        ..SidebarPrefs::default()
    }
}

pub(super) fn normalize_section(s: Option<String>) -> &'static str {
    match s.as_deref() {
        Some("connection") => "connection",
        Some("password") => "password",
        Some("management") => "management",
        _ => "all",
    }
}

#[derive(Deserialize)]
pub(super) struct SettingsQuery {
    pub(super) section: Option<String>,
    /// Outcome flag from the setupbot POST (?setupbot=1 success, any other
    /// value failure) — rendered as a banner by settings_page.
    pub(super) setupbot: Option<String>,
    /// Outcome flag from the MFA mutation POSTs (?mfa=disabled success;
    /// any other value = the re-auth code was rejected, nothing changed) —
    /// rendered as a banner by settings_page.
    pub(super) mfa: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn settings_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SettingsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(q.section);
    let setupbot_banner = match q.setupbot.as_deref() {
        Some("1") => Some(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Bot profile updated — commands, description and short description pushed to Telegram.</div>"#
                .to_string(),
        ),
        Some(_) => Some(
            r#"<div class="alert alert-error" role="alert">Bot profile update failed — Telegram refused or was unreachable. Check the bot token; the profile also refreshes on restart.</div>"#
                .to_string(),
        ),
        None => None,
    };
    let mfa_banner = match q.mfa.as_deref() {
        Some("disabled") => Some(
            r#"<div class="alert alert-success" role="status" aria-live="polite">MFA disabled. Sign-in is password-only until MFA is enrolled again.</div>"#
                .to_string(),
        ),
        Some(_) => Some(
            r#"<div class="alert alert-error" role="alert">That code was not accepted — MFA settings are unchanged. Enter a current authenticator code or an unused recovery code.</div>"#
                .to_string(),
        ),
        None => None,
    };
    let banner = [setupbot_banner, mfa_banner]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    let banner = if banner.is_empty() {
        None
    } else {
        Some(banner)
    };
    Html(html_page(
        &state,
        Some(&session),
        "Settings",
        &settings_body(&state, &session, Some(section), banner.as_deref()),
    ))
    .into_response()
}

/// The settings page body. `banner` carries the outcome of a save (rendered
/// above the panels so a POST can answer in place). Secrets are write-only:
/// the current bot token / Crypto Pay token are never rendered back, only
/// whether one is configured.
pub(super) fn settings_body(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    banner: Option<&str>,
) -> String {
    let banner = banner.unwrap_or_default();
    let sec = section.unwrap_or("all");
    let show_all = sec == "all";
    let show_conn = show_all || sec == "connection";
    let show_pw = show_all || sec == "password";
    let show_mgmt = show_all || sec == "management";

    let tabs = settings_tabs(state, sec);
    let connection_panel = if show_conn {
        connection_panel(state)
    } else {
        String::new()
    };
    let password_panel = if show_pw {
        password_panel(state)
    } else {
        String::new()
    };
    let mfa_panel = mfa_panel(state, session);
    let management_panel = if show_mgmt {
        management_panel(state, session)
    } else {
        String::new()
    };

    let restart = restart_required_banner(state);
    let restart_button = format!(
        r#"<article class="panel"><h3>Restart app</h3><p class="help">Apply pending changes (config, bots, .env edits). The app exits and restarts under systemd/Docker; on desktop reopen it manually.</p><form method="post" action="{}"><button type="submit" class="btn btn-secondary" onclick="return confirm('Restart the app now?')">Restart now</button></form></article>"#,
        state.url("/settings/restart")
    );
    format!(
        r#"{banner}{restart}<h1>Settings <span class="sub">bot connection and console access</span></h1>
{tabs}
{connection_panel}
{password_panel}
{mfa_panel}
{management_panel}
{restart_button}"#,
        banner = banner,
        restart = restart,
        tabs = tabs,
        connection_panel = connection_panel,
        password_panel = password_panel,
        mfa_panel = mfa_panel,
        management_panel = management_panel,
    )
}

fn settings_tabs(state: &WebState, sec: &str) -> String {
    let tab = |label: &str, href: String, active: bool| -> String {
        let cls = if active {
            "btn btn-secondary btn-sm"
        } else {
            "btn btn-ghost btn-sm"
        };
        let aria = if active {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(
            r#"<a class="{}" href="{}"{}>{}</a>"#,
            cls, href, aria, label
        )
    };
    format!(
        r#"<nav class="inline-form" aria-label="Settings sections">
{}{}{}{}
</nav>"#,
        tab("All", state.url("/settings"), sec == "all"),
        tab(
            "Connection",
            state.url("/settings?section=connection"),
            sec == "connection"
        ),
        tab(
            "Password",
            state.url("/settings?section=password"),
            sec == "password"
        ),
        tab(
            "Management",
            state.url("/settings?section=management"),
            sec == "management"
        ),
    )
}

/// The BotFather recipe, duplicated from channel_ops.rs by design (F2.2).
/// `pub(super)`: the multi-bot panel (settings_tokens.rs) renders it too.
pub(super) const BOTFATHER_RECIPE_SETTINGS: &str = r#"<details><summary>How to get a bot token (2 minutes)</summary><ol><li>Open Telegram and message <a href="https://t.me/BotFather" rel="noopener">@BotFather</a>.</li><li>Send <code>/newbot</code>, choose a display name, then a username ending in <code>bot</code>.</li><li>Copy the token it replies with (<code>123456:ABC-…</code>) and paste it below.</li><li>For your owner ID, message <a href="https://t.me/userinfobot" rel="noopener">@userinfobot</a> — it replies with your numeric ID.</li><li>Keep the bot's privacy mode ON; add it as <b>admin</b> to your channel when you sell access.</li></ol></details>"#;

fn connection_panel(state: &WebState) -> String {
    if multi_bot_mode(state) {
        return multi_bot_connection_panel(state);
    }
    let token_badge = if state.cfg.owner_id == 0 {
        r#"<span class="badge badge-warning">not connected</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    };
    let token_help = if state.cfg.owner_id == 0 {
        "From @BotFather. The bot connects on restart once a token is saved. Get one in two minutes: message @BotFather, send /newbot, paste the reply here."
    } else {
        "Leave blank to keep the token you already saved — the console never shows it. To replace it, get a new token from @BotFather and paste it here."
    };
    let crypto_badge = crypto_badge_of(state);
    let owner_id_value = owner_id_value_of(state);
    let default_channel_value = state.db.default_channel().unwrap_or_default();
    format!(
        r#"<details open id="connection"><summary>Bot connection</summary>
<article class="panel">
<p class="help" id="connection-mode">This console is in single-bot mode. The fields below configure that one bot.</p>
<form method="post" action="{}">
<input type="hidden" name="section" value="connection">
<label for="settings-bot-token">Bot token {token_badge}</label>
<span class="secret-field"><input type="password" id="settings-bot-token" name="bot_token" autocomplete="new-password" placeholder="123456:ABC-DEF…">{toggle}</span><span class="help">{token_help}</span>
{recipe}
<label for="owner_id">Owner Telegram user ID</label><input id="owner_id" type="text" name="owner_id" inputmode="numeric" value="{}" placeholder="123456789"><span class="help">Your own Telegram account number (@userinfobot replies with it). Leave blank to keep the current one.</span>
<label for="default_channel">Default channel</label><input id="default_channel" type="text" name="default_channel" value="{}" placeholder="@yourchannel"><span class="help">Where posts and paid access go when nothing else is set — for example @myshopnews. Leave blank to keep the current one.</span>
<label for="settings-crypto-token">Crypto Pay token {crypto_badge}</label>
<span class="secret-field"><input type="password" id="settings-crypto-token" name="crypto_token" autocomplete="new-password" placeholder="from @CryptoBot → Crypto Pay → Create App">{toggle}</span><span class="help">From @CryptoBot → Crypto Pay → Create App. Lets the console verify payments automatically. Leave blank to keep the current token.</span>
<button type="submit">Save settings</button>
</form>
<p class="help">Blank fields change nothing. Saving writes the .env file — changes go live when the app restarts.</p>
<p class="help">AI provider and usage live on the <a href="{}">AI page</a>.</p>
<p class="help">REST API keys are managed on the <a href="{}">API keys</a> page.</p>
</article></details>"#,
        state.url("/settings/save"),
        esc(&owner_id_value),
        esc(&default_channel_value),
        state.url("/ai"),
        state.url("/keys"),
        recipe = BOTFATHER_RECIPE_SETTINGS,
        toggle = SECRET_TOGGLE,
        token_help = token_help,
        token_badge = token_badge,
        crypto_badge = crypto_badge,
    )
}

/// Crypto Pay token presence badge — shared by both connection panels so the
/// markup cannot drift between them.
pub(super) fn crypto_badge_of(state: &WebState) -> String {
    if state
        .cfg
        .crypto_token
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        r#"<span class="badge badge-warning">not set</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    }
}

/// The owner-id field's pre-filled value: the boot-resolved primary owner,
/// blank when unset. Shared by both connection panels.
pub(super) fn owner_id_value_of(state: &WebState) -> String {
    if state.cfg.owner_id != 0 {
        state.cfg.owner_id.to_string()
    } else {
        String::new()
    }
}

fn password_panel(state: &WebState) -> String {
    format!(
        r#"<details open id="password"><summary>Dashboard password</summary>
<article class="panel">
<form method="post" action="{}">
<input type="hidden" name="section" value="password">
<label for="settings-new-password">New password</label>
<span class="secret-field"><input type="password" id="settings-new-password" name="new_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters. Other sessions of this account are signed out; this one stays.</span>
<button type="submit">Change password</button>
</form></article></details>"#,
        state.url("/settings/password"),
        toggle = SECRET_TOGGLE,
    )
}

fn mfa_panel(state: &WebState, session: &Session) -> String {
    let (_secret, enabled, _last) = state.db.get_totp_state(session.account_id);
    let unused = state.db.list_unused_recovery_codes(session.account_id);
    let mfa_status = if enabled {
        r#"<span class="badge badge-success">enabled</span>"#
    } else {
        r#"<span class="badge badge-warning">disabled</span>"#
    };
    format!(
        r#"<details open id="mfa"><summary>Multi-factor authentication (MFA)</summary>
<article class="panel">
<dl class="kv"><dt>Status</dt><dd>{status}</dd><dt>Unused recovery codes</dt><dd class="mono">{unused}</dd></dl>
<form method="post" action="{}"><button type="submit" class="btn btn-sm">Enroll / Re-enroll MFA</button></form>
<form method="post" action="{}">
<label for="mfa-disable-code">Code — to disable MFA</label>
<input type="text" id="mfa-disable-code" name="code" required autocomplete="one-time-code"><span class="help">Required: a hijacked session cannot disable MFA without the current factor.</span>
<button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Disable MFA? You will sign in with password only until you enroll again.')">Disable MFA</button>
</form>
<form method="post" action="{}">
<label for="mfa-recovery-code">Code — to rotate recovery codes</label>
<input type="text" id="mfa-recovery-code" name="code" required autocomplete="one-time-code"><span class="help">Required to rotate. A recovery code is accepted (and consumed) when your authenticator is unavailable.</span>
<button type="submit" class="btn btn-sm">Regenerate recovery codes</button>
</form>
</article></details>"#,
        state.url("/settings/mfa/enroll"),
        state.url("/settings/mfa/disable"),
        state.url("/settings/mfa/recovery"),
        status = mfa_status,
        unused = unused,
    )
}

fn management_panel(state: &WebState, session: &Session) -> String {
    let prefs = effective_sidebar_prefs(state, session);
    let checked = |b: bool| if b { "checked" } else { "" };
    format!(
        r#"<details open id="management"><summary>Management</summary>
<article class="panel">
<p class="help">Open a section to manage that area of the console. Toggle which Workspace links appear in the sidebar.</p>
<form method="post" action="{}">
<input type="hidden" name="section" value="management">
<label><input type="checkbox" name="show_ai" value="1" {}> Show AI in Workspace</label>
<label><input type="checkbox" name="show_crypto" value="1" {}> Show Crypto in Workspace</label>
<label><input type="checkbox" name="show_admins" value="1" {}> Show Admins in Workspace</label>
<label><input type="checkbox" name="show_sellers" value="1" {}> Show Sellers in Workspace</label>
<label><input type="checkbox" name="show_webusers" value="1" {}> Show Web users in Workspace</label>
<button type="submit">Save sidebar</button>
</form>
<div class="grid-2">
<a class="panel" href="{}"><h3>AI</h3><p class="help">Provider, API key, base URL, usage and cost.</p></a>
<a class="panel" href="{}"><h3>Crypto</h3><p class="help">Crypto Pay token and account.</p></a>
<a class="panel" href="{}"><h3>Admins</h3><p class="help">Add and remove console admins.</p></a>
<a class="panel" href="{}"><h3>Sellers</h3><p class="help">Seller accounts and channels.</p></a>
<a class="panel" href="{}"><h3>Web users</h3><p class="help">Console accounts and passwords.</p></a>
</div>
<form method="post" action="{}"><button type="submit" class="btn btn-secondary btn-sm">Push commands &amp; profile to Telegram</button></form>
<p class="help">Updates your bot's command list and description on Telegram. Use this after changing the bot's profile.</p>
<h3>Console session</h3>
<form method="post" action="{}"><button type="submit" class="btn btn-secondary btn-sm">Sign out</button></form>
<p class="help">Sign out of this console. You will need your dashboard password to sign back in.</p>
</article></details>"#,
        state.url("/settings/sidebar"),
        checked(prefs.ai),
        checked(prefs.crypto),
        checked(prefs.admins),
        checked(prefs.sellers),
        checked(prefs.webusers),
        state.url("/ai"),
        state.url("/crypto"),
        state.url("/admins"),
        state.url("/sellers"),
        state.url("/webusers"),
        state.url("/settings/setupbot"),
        state.url("/logout"),
    )
}

/// Owner-only: trigger an in-process restart. Under systemd/Docker (restart
/// policy) the process exits and is brought back up; on desktop the user
/// reopens the app. We delay the exit so the response can flush first.
#[tracing::instrument(skip_all)]
pub(super) async fn settings_restart(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Settings", "Settings");
    }
    state.db.set_config("restart_required", "1");
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        std::process::exit(0);
    });
    let body = format!(
        r#"<h1>Restarting <span class="sub">the app is shutting down</span></h1>
<div class="alert alert-info" role="status" aria-live="polite">The app is restarting. If it doesn't come back automatically within a few seconds, reopen it (desktop) or run <code>systemctl restart telegram-agent</code> / <code>docker compose restart</code>.</div>
<p><a class="btn" href="{}">Back to dashboard</a></p>"#,
        state.url("/dashboard")
    );
    Html(html_page(&state, Some(&session), "Restarting", &body)).into_response()
}

pub(super) fn settings_response(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    banner_html: String,
) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Settings",
        &settings_body(state, session, section, Some(&banner_html)),
    ))
    .into_response()
}

pub(super) fn settings_error(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    msg: &str,
) -> Response {
    settings_response(state, session, section, focused_error_alert(msg))
}

/// Trimmed, non-empty view of an optional form field: the single definition
/// of "blank = keep current" for settings saves.
pub(super) fn non_blank(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Map-flavored form access for the HashMap-extracted save (§3.4): known
/// keys only — a posted key that names no rendered field is ignored, so a
/// crafted `bot_nosuchbot` can never select a bot that is not listed.
pub(super) fn form_value<'a>(form: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    form.get(key).map(String::as_str)
}

/// `non_blank` for a map lookup: the same trim + drop-empty steps (the
/// "blank = keep current" contract), expressed on the borrowed value
/// because the owned clone cannot outlive this function.
pub(super) fn non_blank_in<'a>(form: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    form.get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

#[tracing::instrument(skip_all)]
pub(super) async fn settings_save(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(form_value(&f, "section").map(str::to_string));
    // Multi-bot mode answers from the per-bot panel contract (§5). The
    // legacy TELEGRAM_BOT_TOKEN write below is unreachable there by design —
    // that shadowed key is exactly what made the old banner lie (§0).
    if multi_bot_mode(&state) {
        return settings_save_multi_bot(&state, &session, section, &f).await;
    }
    // Build updates ONLY for non-blank fields: a blank field means "keep the
    // current value", so the secrets already in .env can never be clobbered
    // by an empty form field.
    let mut updates: Vec<(String, String)> = Vec::new();
    if let Some(t) = non_blank_in(&f, "bot_token") {
        // A changed token is validated before anything is written: a typo
        // must never take down the bot's connection on the next restart;
        // the verdict (rejected vs unreachable) decides the copy (F2.5).
        if let Err(e) = validate_token_username(t).await {
            return settings_error(&state, &session, Some(section), &token_error_copy(&e));
        }
        updates.push(("TELEGRAM_BOT_TOKEN".to_string(), t.to_string()));
    }
    if let Some(o) = non_blank_in(&f, "owner_id") {
        match o.parse::<i64>() {
            Ok(v) if v > 0 => updates.push(("OWNER_ID".to_string(), v.to_string())),
            _ => {
                return settings_error(
                    &state,
                    &session,
                    Some(section),
                    "Owner ID must be the number @userinfobot replies with — for example 123456789. Nothing was saved.",
                );
            }
        }
    }
    if let Some(c) = non_blank_in(&f, "default_channel") {
        updates.push(("DEFAULT_CHANNEL".to_string(), c.to_string()));
    }
    if let Some(c) = non_blank_in(&f, "crypto_token") {
        updates.push(("CRYPTO_PAY_TOKEN".to_string(), c.to_string()));
    }
    if updates.is_empty() {
        // Nothing to change — do not touch .env as a side effect of an
        // all-blank submit.
        return settings_response(
            &state,
            &session,
            Some(section),
            r#"<div class="alert alert-info" role="status" aria-live="polite">Nothing to save — all fields were blank.</div>"#
                .to_string(),
        );
    }
    match crate::config::write_env_file(&updates) {
        Ok(_) => {
            // .env changed, but the running process reads its config snapshot
            // once at boot: set the restart flag the /dashboard and /settings
            // banners read (first-run setup sets the same flag).
            state.db.set_config("restart_required", "1");
            settings_response(
                &state,
                &session,
                Some(section),
                r#"<div class="alert alert-success" role="status" aria-live="polite">Saved. The bot connects on restart.</div>
<p class="help">Values are written to .env; the running process keeps the settings snapshot it started with, so restart to apply them.</p>"#
                    .to_string(),
            )
        }
        Err(e) => settings_error(
            &state,
            &session,
            Some(section),
            &format!(
                "Failed to write .env: {}. Nothing was saved.",
                esc(&e.to_string())
            ),
        ),
    }
}

#[derive(Deserialize)]
pub(super) struct SettingsPasswordForm {
    pub(super) new_password: String,
    pub(super) section: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn settings_password(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SettingsPasswordForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(f.section.clone());
    // Mirrors the api.rs auth_password contract exactly, no invention: the
    // owner's web session is the strongest identity a deployment has, so no
    // current password is demanded of it; the 6-character floor is the same.
    if f.new_password.chars().count() < 6 {
        return settings_error(
            &state,
            &session,
            Some(section),
            "Password must be at least 6 characters.",
        );
    }
    if let Err(e) = state
        .db
        .update_web_password_by_id(session.account_id, &f.new_password)
        .await
    {
        return settings_error(
            &state,
            &session,
            Some(section),
            &format!("Could not change the password: {}", esc(&e)),
        );
    }
    // Rotation evicts every OTHER session of this account (a stolen session
    // must not survive it); the caller keeps theirs — same helper api.rs uses.
    if let Some(tok) = caller_token(&headers) {
        state.db.delete_other_web_sessions(session.account_id, &tok);
    }
    settings_response(
        &state,
        &session,
        Some(section),
        r#"<div class="alert alert-success" role="status" aria-live="polite">Password changed. Other sessions were signed out.</div>"#
            .to_string(),
    )
}

#[derive(Deserialize)]
pub(super) struct SidebarForm {
    pub(super) section: Option<String>,
    pub(super) show_ai: Option<String>,
    pub(super) show_crypto: Option<String>,
    pub(super) show_admins: Option<String>,
    pub(super) show_sellers: Option<String>,
    pub(super) show_webusers: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn settings_sidebar(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SidebarForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(f.section.clone());
    let prefs = SidebarPrefs {
        ai: f.show_ai.is_some(),
        crypto: f.show_crypto.is_some(),
        admins: f.show_admins.is_some(),
        sellers: f.show_sellers.is_some(),
        webusers: f.show_webusers.is_some(),
    };
    state.db.set_config("sidebar_items", &prefs.to_json());
    settings_response(
        &state,
        &session,
        Some(section),
        r#"<div class="alert alert-success" role="status" aria-live="polite">Sidebar updated.</div>"#.to_string(),
    )
}

/// The session token behind a request, resolved the way
/// `session_from_headers` does (cookie first, then Bearer) so the password
/// rotation can keep the caller's session while evicting the others.
pub(super) fn caller_token(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(t) = get_cookie(headers) {
        return Some(t);
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string())
}
