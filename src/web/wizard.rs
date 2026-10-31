use super::*;
// ---- Connect wizard (door shell, stateless) ----------------------------------
//
// One step per screen, one POST per step, no auto-advance. The step rendered
// by GET /identity/wizard is derived per request from the actor's status()
// plus the ToS acceptance flag, so back/refresh always show the current step
// and a process restart resets the wizard for free. The credentials step is
// the one deliberate pause: with the boot-frozen config snapshot the running
// actor cannot see new TELEGRAM_API_ID/HASH until restart (same posture as
// settings_save), so the wizard says so instead of pretending.

pub(super) fn wizard_shell(state: &WebState, page: &(String, String)) -> Response {
    Html(html_page_bare_panel(
        &state.base_path,
        &page.0,
        &page.1,
        "panel narrow",
    ))
    .into_response()
}

pub(super) fn wizard_step_for(status: &IdentityStatus, tos_accepted: bool) -> WizardStep {
    match status.state {
        // Connected and persisted: nothing left to do (the GET stays honest
        // on refresh — it never re-runs a completed wizard).
        IdentityState::Ready if status.persisted => WizardStep::Done,
        IdentityState::Ready => WizardStep::Passphrase,
        IdentityState::AwaitingPassword => WizardStep::Password,
        IdentityState::AwaitingCode => WizardStep::Code,
        IdentityState::Configured if tos_accepted => WizardStep::Phone,
        IdentityState::Configured => WizardStep::Accept,
        // Credentials exist only in .env here; until restart the actor still
        // reports NotConfigured/Disconnected, so the wizard stays on step 2.
        IdentityState::NotConfigured | IdentityState::Disconnected if tos_accepted => {
            WizardStep::Credentials
        }
        IdentityState::NotConfigured | IdentityState::Disconnected => WizardStep::Accept,
        IdentityState::Locked => WizardStep::Done,
        IdentityState::Disabled => WizardStep::DisabledInfo,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum WizardStep {
    Accept,
    Credentials,
    Phone,
    Code,
    Password,
    Passphrase,
    DisabledInfo,
    Done,
}

pub(super) fn wizard_accept_body(state: &WebState, alert: Option<String>) -> (String, String) {
    let body = format!(
        r#"{alert}<h1>Connect your personal identity</h1>
<p class="help">Step 1 of 5</p>
<div class="alert alert-info" role="status" aria-live="polite">This console will sign in to Telegram as <b>you</b> — your personal account, not a bot. Posts, replies and reads are attributed to your account and are visible to everyone on Telegram as yours.</div>
<p>Telegram may limit or suspend accounts that automate actions. Anything this console sends — channel posts, replies, reads — is sent by your account and counted against it. Anyone with access to this console can act as you.</p>
<form method="post" action="{}">
<div><input type="checkbox" id="tos-accept" name="accept" value="1" required aria-required="true"> <label for="tos-accept">I understand the risks</label></div>
<div><button type="submit">Continue</button></div>
</form>"#,
        state.url("/identity/wizard/accept"),
        alert = alert.unwrap_or_default(),
    );
    ("Connect identity".to_string(), body)
}

pub(super) fn wizard_credentials_body(state: &WebState, alert: Option<String>) -> (String, String) {
    // The running actor reads its config snapshot once at boot, so freshly
    // written credentials need a restart before the phone step can talk to
    // Telegram (the same honest pause settings_save has).
    let restart_note = if state.db.get_config("restart_required").as_deref() == Some("1") {
        r#"<p class="help">Credentials were saved to .env — restart the app to continue. The wizard resumes at the phone step.</p>"#
    } else {
        ""
    };
    let body = format!(
        r#"{alert}<h1>API credentials</h1>
<p class="help">Step 2 of 5</p>
<form method="post" action="{}">
<label for="connect-api-id">API ID</label>
<input type="text" id="connect-api-id" name="api_id" required inputmode="numeric" autocomplete="off">
<span class="help">From my.telegram.org → API development tools.</span>
<label for="connect-api-hash">API hash</label>
<span class="secret-field"><input type="password" id="connect-api-hash" name="api_hash" required autocomplete="off">{toggle}</span>
<span class="help">From my.telegram.org. Written to .env (0600) — never shown again after this.</span>
{restart_note}
<button type="submit">Continue</button>
</form>"#,
        state.url("/identity/wizard/credentials"),
        toggle = SECRET_TOGGLE,
        alert = alert.unwrap_or_default(),
        restart_note = restart_note,
    );
    ("API credentials".to_string(), body)
}

pub(super) fn wizard_phone_body(
    state: &WebState,
    alert: Option<String>,
    phone: &str,
) -> (String, String) {
    let body = format!(
        r#"{alert}<h1>Your phone number</h1>
<p class="help">Step 3 of 5</p>
<form method="post" action="{}">
<label for="connect-phone">Phone number</label>
<input type="tel" id="connect-phone" name="phone" required inputmode="tel" autocomplete="tel" value="{phone}">
<span class="help">International format, e.g. +49 170 1234567. Telegram delivers the login code to this account's Telegram app.</span>
<div><button type="submit">Send code</button></div>
</form>
<p class="help">Telegram sends the code to your Telegram app first — not by SMS.</p>"#,
        state.url("/identity/wizard/phone"),
        phone = esc(phone),
        alert = alert.unwrap_or_default(),
    );
    ("Your phone number".to_string(), body)
}

pub(super) fn wizard_code_body(state: &WebState, alert: Option<String>) -> (String, String) {
    let body = format!(
        r#"{alert}<h1>Login code</h1>
<p class="help">Step 4 of 5</p>
<p class="help">If your account uses two-factor authentication, one more step follows.</p>
<form method="post" action="{}">
<label for="connect-code">Login code</label>
<input type="text" id="connect-code" name="code" required inputmode="numeric" pattern="[0-9]*" autocomplete="one-time-code">
<span class="help">The code Telegram just sent to your app. It is not your 2FA password.</span>
<div><button type="submit">Verify code</button></div>
</form>"#,
        state.url("/identity/wizard/code"),
        alert = alert.unwrap_or_default(),
    );
    ("Login code".to_string(), body)
}

pub(super) fn wizard_password_body(state: &WebState, alert: Option<String>) -> (String, String) {
    let body = format!(
        r#"{alert}<h1>Two-factor authentication</h1>
<p class="help">Optional — only if your account has two-factor</p>
<div class="alert alert-info" role="status" aria-live="polite">This account is protected with two-factor authentication. Type its password to finish signing in.</div>
<form method="post" action="{}">
<label for="connect-2fa">Telegram 2FA password</label>
<span class="secret-field"><input type="password" id="connect-2fa" name="password" required autocomplete="off">{toggle}</span>
<span class="help">Sent to Telegram to finish this sign-in. Used once and never stored — this is <b>not</b> your unlock passphrase.</span>
<div><button type="submit">Verify password</button></div>
</form>"#,
        state.url("/identity/wizard/password"),
        toggle = SECRET_TOGGLE,
        alert = alert.unwrap_or_default(),
    );
    ("Two-factor authentication".to_string(), body)
}

pub(super) fn wizard_passphrase_body(state: &WebState, alert: Option<String>) -> (String, String) {
    let body = format!(
        r#"{alert}<h1>Unlock passphrase</h1>
<p class="help">Final step</p>
<p>Choose an unlock passphrase. It protects the stored Telegram session at rest — nothing works after a restart until you type it again. It is not stored anywhere and cannot be reset: if you forget it, the stored identity is unreadable and you must disconnect and reconnect.</p>
<form method="post" action="{}">
<label for="connect-passphrase">Unlock passphrase</label>
<span class="secret-field"><input type="password" id="connect-passphrase" name="passphrase" required minlength="8" autocomplete="off">{toggle}</span>
<span class="help">This becomes the key that encrypts your stored session.</span>
<label for="connect-passphrase-confirm">Confirm passphrase</label>
<span class="secret-field"><input type="password" id="connect-passphrase-confirm" name="confirm_passphrase" required autocomplete="off">{toggle}</span>
<span class="help">Typed twice, like a password.</span>
<div><button type="submit">Connect identity</button></div>
</form>"#,
        state.url("/identity/wizard/passphrase"),
        toggle = SECRET_TOGGLE,
        alert = alert.unwrap_or_default(),
    );
    ("Unlock passphrase".to_string(), body)
}

pub(super) fn wizard_disabled_body(state: &WebState) -> (String, String) {
    let body = format!(
        r#"<h1>Identity unavailable</h1>
<p>Telegram reported this session's authorization key is used by another client, so the identity is disabled. Nothing can connect until the stored session is removed and the identity is paired again.</p>
<p class="help">Re-pairing starts the wizard over. The Identity page shows the current state.</p>
<p><a class="btn" href="{}">Back to Identity</a></p>"#,
        state.url("/identity")
    );
    ("Identity unavailable".to_string(), body)
}

/// GET /identity/wizard — renders the step the actor's status() implies.
/// Idempotent and back/refresh safe: no step ever advances without its POST.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let status = state.identity.status().await;
    let tos = state.db.get_config("identity_tos_accepted").is_some();
    match wizard_step_for(&status, tos) {
        WizardStep::Done => Redirect::to(&state.url("/identity")).into_response(),
        WizardStep::DisabledInfo => wizard_shell(&state, &wizard_disabled_body(&state)),
        WizardStep::Accept => wizard_shell(&state, &wizard_accept_body(&state, None)),
        WizardStep::Credentials => wizard_shell(&state, &wizard_credentials_body(&state, None)),
        WizardStep::Phone => wizard_shell(&state, &wizard_phone_body(&state, None, "")),
        WizardStep::Code => wizard_shell(&state, &wizard_code_body(&state, None)),
        WizardStep::Password => wizard_shell(&state, &wizard_password_body(&state, None)),
        WizardStep::Passphrase => wizard_shell(&state, &wizard_passphrase_body(&state, None)),
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityAcceptForm {
    pub(super) accept: Option<String>,
}

/// Step 1 — record the risk acceptance. An unchecked box that reaches the
/// server re-renders with the §3.8 copy and changes nothing.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_accept(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityAcceptForm>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Identity") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if f.accept.as_deref().filter(|s| !s.is_empty()).is_none() {
        let alert = focused_error_alert(
            "You must accept the risks before connecting. Nothing was sent to Telegram.",
        );
        return wizard_shell(&state, &wizard_accept_body(&state, Some(alert)));
    }
    state.db.set_config("identity_tos_accepted", "1");
    state.db.log_action(
        session.account_id,
        &session.role,
        "identity_tos_accepted",
        "identity",
        "0",
        None,
    );
    Redirect::to(&state.url("/identity/wizard")).into_response()
}

#[derive(Deserialize)]
pub(super) struct IdentityCredentialsForm {
    pub(super) api_id: String,
    pub(super) api_hash: String,
}

/// Step 2 — api_id + api_hash, persisted through the shared key-preserving
/// .env writer (the one settings_save uses) as TELEGRAM_API_ID/HASH.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_credentials(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityCredentialsForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let status = state.identity.status().await;
    // Already configured (or further along): nothing to persist.
    if matches!(
        status.state,
        IdentityState::Configured
            | IdentityState::AwaitingCode
            | IdentityState::AwaitingPassword
            | IdentityState::Ready
            | IdentityState::Locked
    ) {
        return Redirect::to(&state.url("/identity/wizard")).into_response();
    }
    // ToS first: nothing is written before the risk acceptance exists.
    if state.db.get_config("identity_tos_accepted").is_none() {
        return Redirect::to(&state.url("/identity/wizard")).into_response();
    }
    let render = |alert: String| {
        let page = wizard_credentials_body(&state, Some(alert));
        wizard_shell(&state, &page)
    };
    let api_id = f.api_id.trim();
    let api_hash = f.api_hash.trim();
    if !api_id.parse::<i32>().map(|v| v != 0).unwrap_or(false) {
        return render(focused_error_alert(
            "API ID must be the number shown at my.telegram.org → API development tools. Nothing was saved.",
        ));
    }
    if api_hash.is_empty() {
        return render(focused_error_alert(
            "Paste the API hash from my.telegram.org. Nothing was saved.",
        ));
    }
    match crate::config::write_env_file_replacing(&[
        ("TELEGRAM_API_ID", Some(api_id)),
        ("TELEGRAM_API_HASH", Some(api_hash)),
    ]) {
        Ok(_) => {
            // .env changed; the running process keeps its boot snapshot, so
            // set the restart flag the console banners read.
            state.db.set_config("restart_required", "1");
            Redirect::to(&state.url("/identity/wizard")).into_response()
        }
        Err(e) => render(focused_error_alert(&format!(
            "Failed to write .env: {}. Nothing was saved.",
            esc(&e.to_string())
        ))),
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityPhoneForm {
    pub(super) phone: String,
}

/// Step 3 — ask Telegram to deliver the login code. The phone is preserved
/// on the error re-render (it is not a secret field).
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_phone(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityPhoneForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    if state.db.get_config("identity_tos_accepted").is_none() {
        return Redirect::to(&state.url("/identity/wizard")).into_response();
    }
    let phone = f.phone.trim().to_string();
    if phone.is_empty() {
        let alert = focused_error_alert(
            "That phone number doesn't look right. Use international format, e.g. +49 170 1234567.",
        );
        return wizard_shell(&state, &wizard_phone_body(&state, Some(alert), ""));
    }
    match state.identity.request_code(&phone).await {
        Ok(()) => Redirect::to(&state.url("/identity/wizard")).into_response(),
        Err(e) => {
            let alert = identity_error_html(&e);
            wizard_shell(&state, &wizard_phone_body(&state, Some(alert), &phone))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityCodeForm {
    pub(super) code: String,
}

/// Step 4 — verify the login code. A PasswordRequired result PRGs back to
/// the wizard, which now renders the conditional 2FA step (5a).
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_code(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityCodeForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let code = f.code.trim().to_string();
    if code.is_empty() {
        let alert =
            focused_error_alert("Enter the code Telegram just sent to your app. Nothing was sent.");
        return wizard_shell(&state, &wizard_code_body(&state, Some(alert)));
    }
    match state.identity.sign_in(&code).await {
        Ok(_) => Redirect::to(&state.url("/identity/wizard")).into_response(),
        Err(e) => {
            let alert = identity_error_html(&e);
            wizard_shell(&state, &wizard_code_body(&state, Some(alert)))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityPasswordForm {
    pub(super) password: String,
}

/// Step 5a — the one-shot Telegram 2FA password. It exists for exactly this
/// POST, is forwarded to the actor, and is dropped — never stored, never
/// logged, never re-rendered.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_password(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityPasswordForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let password = f.password;
    if password.is_empty() {
        let alert = focused_error_alert(
            "Type the account's 2FA password to finish signing in. Nothing was sent.",
        );
        return wizard_shell(&state, &wizard_password_body(&state, Some(alert)));
    }
    match state.identity.check_password(&password).await {
        Ok(_) => Redirect::to(&state.url("/identity/wizard")).into_response(),
        Err(e) => {
            let alert = identity_error_html(&e);
            wizard_shell(&state, &wizard_password_body(&state, Some(alert)))
        }
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityPassphraseForm {
    pub(super) passphrase: String,
    pub(super) confirm_passphrase: String,
}

/// Step 5b — set the unlock passphrase and persist the session. Success
/// completes the connection; the passphrase itself is never stored.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wizard_passphrase(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityPassphraseForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let passphrase = f.passphrase;
    let render = |alert: String| {
        let page = wizard_passphrase_body(&state, Some(alert));
        wizard_shell(&state, &page)
    };
    if passphrase.chars().count() < 8 {
        return render(focused_error_alert("Use at least 8 characters."));
    }
    // Upper bound (F9): honest refusal, no actor call.
    if passphrase.chars().count() > PASSPHRASE_MAX_CHARS {
        return render(focused_error_alert(
            "Passphrase must be at most 128 characters.",
        ));
    }
    if passphrase != f.confirm_passphrase {
        return render(focused_error_alert("Passphrases do not match."));
    }
    match state.identity.set_passphrase_and_persist(&passphrase).await {
        Ok(()) => Redirect::to(&state.url("/identity?connected=1")).into_response(),
        Err(e) => render(identity_error_html(&e)),
    }
}
