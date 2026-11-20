use super::*;
// ---------- Web users ----------

// ---------- MFA / TOTP ----------

pub(super) fn pending_from_headers(
    state: &WebState,
    headers: &axum::http::HeaderMap,
) -> Option<(String, i64, String, Option<String>, String)> {
    let raw = get_pending_cookie(headers)?;
    let hash = sha256_hex(&raw);
    let (account_id, purpose, secret, expires) = state.db.get_pending_mfa(&hash)?;
    // expiry check
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&expires)
        && chrono::Utc::now() > dt.with_timezone(&chrono::Utc)
    {
        let _ = state.db.delete_pending_mfa(&hash);
        return None;
    }
    Some((raw, account_id, purpose, secret, hash))
}

#[tracing::instrument(skip_all)]
pub(super) async fn mfa_verify_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Some((_raw, account_id, purpose, _secret, _hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "verify" {
        return Redirect::to(&state.url("/mfa/enroll")).into_response();
    }
    let body = format!(
        r#"<h1>Verify MFA <span class="sub">enter your authenticator code</span></h1>
<article class="panel">
<form method="post" action="{}">
<label>Authenticator code or recovery code<input type="text" name="code" required autocomplete="one-time-code"></label>
<button type="submit">Verify</button>
</form>
<p class="help">Account {}.</p>
</article>"#,
        state.url("/mfa/verify"),
        account_id
    );
    Html(html_page_bare(&state.base_path, "MFA verify", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct MfaCodeForm {
    pub(super) code: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn mfa_verify_submit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let Some((raw, account_id, purpose, _secret, hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "verify" {
        return Redirect::to(&state.url("/mfa/enroll")).into_response();
    }
    // rate limit
    if login_throttle(&state.login_attempts, &format!("mfa:{}", raw), 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "MFA verify",
                "Too many attempts.",
            )),
        )
            .into_response();
    }
    let code = f.code.trim();
    let (secret, enabled, last_used) = state.db.get_totp_state(account_id);
    let mut ok = false;
    if enabled && code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6 {
        let now_ts = chrono::Utc::now().timestamp() as u64;
        for delta in [-30i64, 0i64, 30i64] {
            let ts = (now_ts as i64 + delta).max(0) as u64;
            let counter = (ts / 30) as i64;
            let generated =
                crate::util::totp_generate(&secret.clone().unwrap_or_default(), ts, 6usize, 30u64);
            if crate::util::ct_eq(&generated, code)
                && last_used.map(|l| counter > l).unwrap_or(true)
            {
                ok = true;
                let _ = state.db.update_totp_last_used(account_id, counter);
                break;
            }
        }
    }
    if !ok && state.db.verify_recovery_code(account_id, code) {
        ok = true;
    }
    if ok {
        login_ok(&state.login_attempts, &format!("mfa:{}", raw));
        let _ = state.db.delete_pending_mfa(&hash);
        let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
            account_id,
            String::new(),
            String::new(),
            "seller".to_string(),
            None,
        ));
        let token = state.db.create_web_session(id, &role, seller_id, 7);
        // AppendHeaders: an array of pairs replaces duplicate header names
        // (last one wins) — the session + cleared-pending pair needs append.
        return (
            AppendHeaders([
                (header::SET_COOKIE, session_cookie(&state, &token)),
                (header::SET_COOKIE, clear_pending_cookie(&state)),
            ]),
            Redirect::to(&state.url("/dashboard")),
        )
            .into_response();
    }
    let body = format!(
        r#"<h1>Verify MFA</h1><div class="alert alert-error" role="alert">Invalid code.</div>
<article class="panel"><form method="post" action="{}"><label>Code<input type="text" name="code" required></label><button type="submit">Verify</button></form></article>"#,
        state.url("/mfa/verify")
    );
    Html(html_page_bare(&state.base_path, "MFA verify", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn mfa_enroll_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Some((_raw, account_id, purpose, secret, _hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "enroll" {
        return Redirect::to(&state.url("/mfa/verify")).into_response();
    }
    let secret = secret.unwrap_or_default();
    let username = state
        .db
        .get_web_account_by_id(account_id)
        .map(|r| r.1)
        .unwrap_or_default();
    let otpauth = format!(
        "otpauth://totp/TelegramBusiness:{}?secret={}&issuer=TelegramBusiness&algorithm=SHA256&digits=6&period=30",
        username, secret
    );
    let qr_svg = crate::qr::qr_svg_scaled(&otpauth, 6);
    let body = format!(
        r#"<h1>Enroll MFA <span class="sub">scan with your authenticator</span></h1>
<article class="panel">
<div class="qr" role="img" aria-label="QR code for your authenticator app">{qr}</div>
<p class="help">Can't scan? Enter this key in your authenticator app manually.</p>
<p><code class="copyable">{otpauth}</code></p>
<form method="post" action="{action}">
<label>Enter code from app<input type="text" name="code" required autocomplete="one-time-code"></label>
<button type="submit">Enable MFA</button>
</form>
</article>"#,
        qr = qr_svg,
        otpauth = esc(&otpauth),
        action = state.url("/mfa/enroll")
    );
    Html(html_page_bare(&state.base_path, "MFA enroll", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn mfa_enroll_submit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let Some((raw, account_id, purpose, secret_opt, hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "enroll" {
        return Redirect::to(&state.url("/mfa/verify")).into_response();
    }
    let secret = secret_opt.unwrap_or_default();
    if login_throttle(&state.login_attempts, &format!("mfa:{}", raw), 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "MFA enroll",
                "Too many attempts.",
            )),
        )
            .into_response();
    }
    let code = f.code.trim();
    let now_ts = chrono::Utc::now().timestamp() as u64;
    // Allow ±1 30s window to absorb clock skew between the server and the
    // authenticator (RFC 6238 §5.2). Strict single-window matching caused
    // valid codes to be rejected at the window boundary.
    let mut ok = false;
    for delta in [-30i64, 0i64, 30i64] {
        let ts = (now_ts as i64 + delta).max(0) as u64;
        let generated = crate::util::totp_generate(&secret.clone(), ts, 6usize, 30u64);
        if crate::util::ct_eq(&generated, code) {
            ok = true;
            break;
        }
    }
    if !ok {
        let body = format!(
            r#"<h1>Enroll MFA</h1><div class="alert alert-error" role="alert">Invalid code.</div>
<article class="panel"><form method="post" action="{}"><label>Code<input type="text" name="code" required></label><button type="submit">Enable</button></form></article>"#,
            state.url("/mfa/enroll")
        );
        return Html(html_page_bare(&state.base_path, "MFA enroll", &body)).into_response();
    }
    login_ok(&state.login_attempts, &format!("mfa:{}", raw));
    let _ = state.db.enable_totp(account_id, secret.clone());
    // generate recovery codes
    let mut plain: Vec<String> = Vec::new();
    for _ in 0..10 {
        plain.push(crate::util::gen_recovery_code());
    }
    let plain_clone = plain.clone();
    let hashed = tokio::task::spawn_blocking(move || {
        plain_clone
            .iter()
            .map(|c| bcrypt::hash(c, bcrypt::DEFAULT_COST).unwrap_or_default())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let _ = state.db.create_recovery_codes(account_id, hashed);
    let _ = state.db.delete_pending_mfa(&hash);
    let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
        account_id,
        String::new(),
        String::new(),
        "seller".to_string(),
        None,
    ));
    let token = state.db.create_web_session(id, &role, seller_id, 7);
    let codes_html = plain
        .iter()
        .map(|c| format!("<li><code class=\"copyable\">{}</code></li>", esc(c)))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        r#"<h1>MFA enabled</h1><div class="alert alert-warn" role="status" aria-live="polite">Save these recovery codes; they will not be shown again.</div>
<ul>{}</ul>
<p><a href="{}">Continue to dashboard</a></p>"#,
        codes_html,
        state.url("/dashboard")
    );
    // Two Set-Cookie headers on one response: an ARRAY of (name, value)
    // pairs would replace duplicate header names (last one wins), so the
    // append form is required for the session + cleared-pending pair.
    (
        AppendHeaders([
            (header::SET_COOKIE, session_cookie(&state, &token)),
            (header::SET_COOKIE, clear_pending_cookie(&state)),
        ]),
        Html(html_page_bare(&state.base_path, "MFA enabled", &body)),
    )
        .into_response()
}

// ---------- Setup links: /setup/claim (SETUP_MCP_UX_SPEC §§2-4) ----------

#[derive(Deserialize)]
pub(super) struct SetupClaimQuery {
    pub(super) token: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SetupClaimForm {
    pub(super) token: String,
    pub(super) password: String,
    pub(super) confirm_password: String,
}

/// Read-only validation of a setup link (GET, §2.4): hash + purpose
/// ('setup_link') + expiry, WITHOUT consuming — a double-clicked or
/// preview-fetched link still renders a working form. Uses the pending-row
/// lookup keyed by hash (`get_pending_mfa`); no consume method is called.
pub(super) fn setup_token_valid(state: &WebState, raw: &str) -> bool {
    let hash = sha256_hex(raw);
    match state.db.get_pending_mfa(hash) {
        Some((_account_id, purpose, _secret, expires_at)) => {
            purpose == crate::db::SETUP_LINK_PURPOSE
                && chrono::DateTime::parse_from_rfc3339(&expires_at)
                    .map(|dt| dt.with_timezone(&chrono::Utc) > chrono::Utc::now())
                    .unwrap_or(false)
        }
        None => false,
    }
}

/// The one generic failure page (§4.1): expired, reused, unknown and
/// wrong-purpose tokens all render this — same markup, same 200 status, no
/// oracle. The message names the fix, not the cause.
pub(super) fn setup_claim_invalid_page(state: &WebState) -> Response {
    let body = r#"<h1>This link has expired</h1>
<div class="alert alert-error" role="alert">This link is invalid or has expired.</div>
<p class="help">Links work once and stop after 15 minutes. Open your Telegram bot and send /dashboard to get a new one.</p>"#;
    Html(html_page_bare_panel(
        &state.base_path,
        "Setup link",
        body,
        "panel narrow",
    ))
    .into_response()
}

/// The claim form (§2.2): the claim_form_html idiom — `<label for>` plus a
/// separate `.secret-field` span because the toggle button must sit inside
/// the field wrapper. The hidden token carries the claim across failed
/// POSTs (a mistyped confirm must not force a new link); password fields
/// are never re-populated. A failed re-render's alert takes focus on load
/// (§5.3, script-free via autofocus on the tabindex="-1" banner).
pub(super) fn setup_claim_form_html(state: &WebState, alert: Option<&str>, token: &str) -> String {
    let alert = alert
        .map(|a| {
            format!(
                r#"<div class="alert alert-error" role="alert" tabindex="-1" autofocus>{}</div>"#,
                a
            )
        })
        .unwrap_or_default();
    format!(
        r#"{alert}<h1>Set up your dashboard</h1>
<p class="help">This one-time link came from your Telegram bot. It expires 15 minutes after it was sent and works once.</p>
<form method="post" action="{}">
<input type="hidden" name="token" value="{}">
<label for="setup-claim-password">New password</label>
<span class="secret-field"><input type="password" id="setup-claim-password" name="password" required minlength="6" aria-required="true" autocomplete="new-password" aria-describedby="setup-claim-strength">{toggle}</span>
<label for="setup-claim-confirm">Confirm password</label>
<span class="secret-field"><input type="password" id="setup-claim-confirm" name="confirm_password" required minlength="6" aria-required="true" autocomplete="new-password" aria-describedby="setup-claim-help">{toggle}</span><span class="help" id="setup-claim-help">At least 6 characters, typed twice.</span>
<p class="help" id="setup-claim-strength">Use a passphrase you don't use anywhere else — 12+ characters is recommended. This console can hold bot, payment and AI credentials.</p>
<button type="submit">Set password and sign in</button>
</form>
<p class="help">Not expecting this? Ignore it — the link stops working after 15 minutes or one use, whichever comes first.</p>"#,
        state.url("/setup/claim"),
        esc(token),
        toggle = SECRET_TOGGLE,
    )
}

pub(super) fn setup_claim_error(state: &WebState, token: &str, msg: &str) -> Response {
    Html(html_page_bare_panel(
        &state.base_path,
        "Set up dashboard",
        &setup_claim_form_html(state, Some(msg), token),
        "panel narrow",
    ))
    .into_response()
}

/// GET /setup/claim?token=… (public, §1.1): validate without consuming,
/// render the form; anything else gets the one generic failure page — a
/// bare GET (missing token) included (§4.2). The raw token is never
/// rendered as text, never logged, never echoed in an error (§2.4).
#[tracing::instrument(skip_all)]
pub(super) async fn setup_claim_page(
    State(state): State<WebState>,
    Query(q): Query<SetupClaimQuery>,
) -> impl IntoResponse {
    let valid = q
        .token
        .as_deref()
        .map(|t| setup_token_valid(&state, t))
        .unwrap_or(false);
    if !valid {
        return setup_claim_invalid_page(&state);
    }
    let form = setup_claim_form_html(&state, None, q.token.as_deref().unwrap_or_default());
    Html(html_page_bare_panel(
        &state.base_path,
        "Set up dashboard",
        &form,
        "panel narrow",
    ))
    .into_response()
}

/// POST /setup/claim (public, §3.1) — all-or-nothing order: throttle keyed
/// on the token's SHA-256, never the raw bearer (login-class: token
/// guessing IS password guessing),
/// then the cheap field checks, THEN the atomic single-use consume, then
/// set password, invalidate the account's other sessions, create the
/// session, auto-login. The token and the password are never logged and
/// never rendered outside the unavoidable hidden field.
#[tracing::instrument(skip_all)]
pub(super) async fn setup_claim_submit(
    State(state): State<WebState>,
    Form(f): Form<SetupClaimForm>,
) -> impl IntoResponse {
    let throttle_key = format!("setup:{}", sha256_hex(&f.token));
    // 1. Throttle (5 / 900 s, shared with sign-in): on trip the form
    // re-renders with the login page's exact banner and the hidden token
    // preserved — no consumption on a throttled request (§3.1).
    if login_throttle(&state.login_attempts, &throttle_key, 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare_panel(
                &state.base_path,
                "Set up dashboard",
                &setup_claim_form_html(
                    &state,
                    Some("Too many attempts. Try again in a few minutes."),
                    &f.token,
                ),
                "panel narrow",
            )),
        )
            .into_response();
    }
    // 2. Field checks before any consume: a fat-fingered confirm must not
    // burn a live token (§3.1). Same server-side 6-char floor as
    // settings_password; the minlength attribute is a courtesy, never the gate.
    if f.password.chars().count() < 6 {
        return setup_claim_error(&state, &f.token, "Passwords must be at least 6 characters.");
    }
    if f.confirm_password != f.password {
        return setup_claim_error(&state, &f.token, "Passwords do not match.");
    }
    // 3. Consume — atomic single-use (DELETE…RETURNING): exactly one
    // concurrent caller wins; expired, reused, unknown and wrong-purpose
    // tokens are all None, all landing on the one generic page.
    let Some(account_id) = state
        .db
        .consume_pending_token(&f.token, crate::db::SETUP_LINK_PURPOSE)
    else {
        return setup_claim_invalid_page(&state);
    };
    // 4. Set the password. Failure here is the deleted-account edge (§4.2):
    // the generic page, no account-existence leak. The token is already
    // consumed — the link is dead either way.
    if state
        .db
        .update_web_password_by_id(account_id, &f.password)
        .await
        .is_err()
    {
        return setup_claim_invalid_page(&state);
    }
    let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
        account_id,
        String::new(),
        String::new(),
        "seller".to_string(),
        None,
    ));
    let token = state.db.create_web_session(id, &role, seller_id, 7);
    // The flow is functionally a password reset (§6.4): every OTHER session
    // of the account is signed out — the freshly minted session is the
    // keeper (the settings_password semantics, with the new session as the
    // caller's since the claim request itself carries none).
    state.db.delete_other_web_sessions(account_id, &token);
    login_ok(&state.login_attempts, &throttle_key);
    (
        [(header::SET_COOKIE, session_cookie(&state, &token))],
        Redirect::to(&state.url("/dashboard?welcome=1")),
    )
        .into_response()
}

// Settings MFA handlers

/// Validates the CURRENT second factor for an MFA-setting change (disable or
/// recovery-code rotation): a valid TOTP code against the account's enrolled
/// secret — same 6-digit window and replay rules as `/mfa/verify` — or, only
/// when the TOTP does not match, a valid unused recovery code (which
/// `verify_recovery_code` consumes on success). TOTP is tried first, so a
/// working authenticator never burns a recovery code; a recovery code is
/// consumed only when it is the factor that actually authorized the change.
pub(super) fn verify_current_second_factor(state: &WebState, account_id: i64, code: &str) -> bool {
    let code = code.trim();
    let (secret, enabled, last_used) = state.db.get_totp_state(account_id);
    if enabled && code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6 {
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let current_counter = (now_ts / 30) as i64;
        let generated =
            crate::util::totp_generate(&secret.clone().unwrap_or_default(), now_ts, 6usize, 30u64);
        if crate::util::ct_eq(&generated, code)
            && last_used.map(|l| current_counter > l).unwrap_or(true)
        {
            let _ = state.db.update_totp_last_used(account_id, current_counter);
            return true;
        }
    }
    state.db.verify_recovery_code(account_id, code)
}

#[tracing::instrument(skip_all)]
pub(super) async fn settings_mfa_enroll(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // create pending enroll
    let mut buf = [0u8; 20];
    let _ = getrandom::fill(&mut buf);
    let secret = crate::util::base32_encode(&buf);
    let pending_raw = crate::util::gen_token(48);
    let pending_hash = sha256_hex(&pending_raw);
    let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let _ = state.db.create_pending_mfa(
        pending_hash,
        session.account_id,
        "enroll",
        Some(secret),
        expires,
    );
    (
        [(header::SET_COOKIE, pending_cookie(&state, &pending_raw))],
        Redirect::to(&state.url("/mfa/enroll")),
    )
        .into_response()
}

/// Disable MFA for the signed-in account. A hijacked session must not be
/// able to strip the second factor: the form requires the CURRENT factor
/// (valid TOTP or unused recovery code, `verify_current_second_factor`).
/// On a rejected code nothing changes and the settings page answers with an
/// error banner (`?mfa=reauth`).
#[tracing::instrument(skip_all)]
pub(super) async fn settings_mfa_disable(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !verify_current_second_factor(&state, session.account_id, &f.code) {
        // No state change on a rejected factor: MFA stays exactly as it was.
        return Redirect::to(&state.url("/settings?mfa=reauth")).into_response();
    }
    let _ = state.db.disable_totp(session.account_id);
    Redirect::to(&state.url("/settings?mfa=disabled")).into_response()
}

/// Regenerate the account's recovery codes. Same re-auth rule as disable:
/// the CURRENT factor is required (TOTP preferred; a recovery code is
/// consumed when it is what authorized the rotation), and a rejected code
/// leaves the old codes untouched.
#[tracing::instrument(skip_all)]
pub(super) async fn settings_mfa_recovery(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !verify_current_second_factor(&state, session.account_id, &f.code) {
        return Redirect::to(&state.url("/settings?mfa=reauth")).into_response();
    }
    let mut plain: Vec<String> = Vec::new();
    for _ in 0..10 {
        plain.push(crate::util::gen_recovery_code());
    }
    let plain_clone = plain.clone();
    let hashed = tokio::task::spawn_blocking(move || {
        plain_clone
            .iter()
            .map(|c| bcrypt::hash(c, bcrypt::DEFAULT_COST).unwrap_or_default())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    // replace existing recovery codes only
    let _ = state.db.delete_recovery_codes(session.account_id);
    let _ = state.db.create_recovery_codes(session.account_id, hashed);
    let codes_html = plain
        .iter()
        .map(|c| format!("<li><code class=\"copyable\">{}</code></li>", esc(c)))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        r#"<h1>Recovery codes</h1><div class="alert alert-warn" role="status" aria-live="polite">New recovery codes (shown once):<ul>{}</ul></div>"#,
        codes_html
    );
    Html(html_page(&state, Some(&session), "Settings", &body)).into_response()
}

/// Web-users flash keys (EDIT_FLOW_SPEC E11): every mutation lands on a
/// banner; `u` carries the non-sensitive username the created / MFA banners
/// name (the bookings `b` object-key precedent).
#[derive(Deserialize)]
pub(super) struct WebUsersQuery {
    pub(super) created: Option<String>,
    pub(super) deleted: Option<String>,
    pub(super) password: Option<String>,
    pub(super) mfa: Option<String>,
    pub(super) error: Option<String>,
    pub(super) u: Option<String>,
}

fn webusers_banner(q: &WebUsersQuery) -> String {
    let who = esc(q.u.as_deref().unwrap_or(""));
    let err = match q.error.as_deref() {
        Some("taken") => Some("That username is taken."),
        Some("create") => Some("Account could not be created — check the values and try again."),
        Some("password") => Some("Password reset failed — try again."),
        _ => None,
    };
    if let Some(msg) = err {
        return format!(r#"<div class="alert alert-error" role="alert">{msg}</div>"#);
    }
    if q.created.as_deref() == Some("1") && !who.is_empty() {
        return flash_success(&format!("Account {who} created."));
    }
    if q.created.as_deref() == Some("1") {
        return flash_success("Account created.");
    }
    if q.deleted.as_deref() == Some("1") {
        return flash_success("Account deleted.");
    }
    if q.password.as_deref() == Some("1") {
        return flash_success("Password reset.");
    }
    if q.mfa.as_deref() == Some("1") && !who.is_empty() {
        return flash_success(&format!("MFA disabled for {who}."));
    }
    if q.mfa.as_deref() == Some("1") {
        return flash_success("MFA disabled.");
    }
    String::new()
}

#[tracing::instrument(skip_all)]
pub(super) async fn webusers_resetmfa(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        // Resolve the username BEFORE the disable so the banner can name
        // the account (E11); an empty name falls back to the who-less
        // banner via `u=`.
        let uname = state
            .db
            .get_web_account_by_id(id)
            .map(|r| r.1)
            .unwrap_or_default();
        let _ = state.db.disable_totp(id);
        return Redirect::to(&state.url(&format!("/webusers?mfa=1&u={}", qenc(uname.trim()))))
            .into_response();
    }
    Redirect::to(&state.url("/webusers")).into_response()
}

/// E13 (EDIT_FLOW_SPEC): the create form's seller picker — the real seller
/// list with channels (the sellers_page idiom), never a typed raw id the
/// owner cannot know. The empty option is platform-level (None).
fn webusers_seller_options(state: &WebState) -> String {
    let mut options = String::from(r#"<option value="">No seller (platform-level)</option>"#);
    for sid in state.db.list_sellers() {
        let ch = state.db.seller_channel(sid).unwrap_or_default();
        let label = if ch.is_empty() {
            "no channel"
        } else {
            ch.as_str()
        };
        options.push_str(&format!(
            r#"<option value="{sid}">{sid} — {}</option>"#,
            esc(label)
        ));
    }
    options
}

#[tracing::instrument(skip_all)]
pub(super) async fn webusers_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<WebUsersQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    let accounts = state.db.list_web_accounts_full();
    let rows = accounts
        .iter()
        .map(|(id, uname, role, seller)| {
            let (_s, enabled, _l) = state.db.get_totp_state(*id);
            // The settings MFA badge idiom (dot before the word), not a bare
            // status word (B15.1).
            let mfa = if enabled {
                r#"<span class="badge badge-success">enabled</span>"#
            } else {
                r#"<span class="badge badge-warning">disabled</span>"#
            };
            // E12: disabling a second factor is keys-revoke-class (danger
            // weight + confirm naming the account); C9: the delete confirm
            // names the username, not the row id.
            let u = esc(uname);
            let role = esc(role);
            let seller_cell = seller.map(|v| v.to_string()).unwrap_or_else(|| "—".into());
            let pw = state.url("/webusers/password");
            let del = state.url("/webusers/delete");
            let rmf = state.url("/webusers/resetmfa");
            let toggle = SECRET_TOGGLE;
            format!(
                r#"<tr><td class="mono">{id}</td><td>{u}</td><td>{role}</td><td class="mono">{seller_cell}</td><td>{mfa}</td><td>
<form method="post" action="{pw}" class="inline-form"><input type="hidden" name="id" value="{id}"><span class="secret-field"><input type="password" name="password" placeholder="new password" aria-label="New password for {u}">{toggle}</span><button type="submit" class="btn btn-sm">Reset</button></form>
<form method="post" action="{del}" class="inline-form" onsubmit="var b=this.querySelector('button[type=submit]');if(b){{b.disabled=true;b.setAttribute('aria-busy','true');}}"><input type="hidden" name="id" value="{id}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Delete account {u}" onclick="return confirm('Delete account {u}? Their sessions end immediately.')">Delete</button></form>
<form method="post" action="{rmf}" class="inline-form"><input type="hidden" name="id" value="{id}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Reset MFA for {u}? They will sign in with password only until they enroll again.')">Reset MFA</button></form>
</td></tr>"#
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let tbody = if rows.is_empty() {
        r#"<tr><td colspan="6" class="empty"><b>No web users.</b></td></tr>"#.to_string()
    } else {
        rows
    };
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Web users"><table aria-label="Web users"><thead><tr><th scope="col">ID</th><th scope="col">Username</th><th scope="col">Role</th><th scope="col">Seller</th><th scope="col">MFA</th><th scope="col">Actions</th></tr></thead><tbody>{tbody}</tbody></table></div>"#
    );
    let banner = webusers_banner(&q);
    let seller_options = webusers_seller_options(&state);
    let create_url = state.url("/webusers/create");
    let toggle = SECRET_TOGGLE;
    let body = format!(
        r#"<h1>Web users <span class="sub">console accounts</span></h1>
{banner}
<article class="panel"><h3>Create account</h3>
<form method="post" action="{create_url}" class="inline-form">
<label for="wu-username">Username</label>
<input type="text" id="wu-username" name="username" placeholder="username" required>
<label for="wu-password">Password</label>
<span class="secret-field"><input type="password" id="wu-password" name="password" placeholder="password" required>{toggle}</span>
<label for="wu-role">Role</label>
<select id="wu-role" name="role"><option value="seller">seller</option><option value="owner">owner</option></select>
<label for="wu-seller-id">Seller</label>
<select id="wu-seller-id" name="seller_id">{seller_options}</select>
<button type="submit">Create</button>
</form></article>
{table}"#
    );
    Html(html_page(&state, Some(&session), "Web users", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct WebUserCreateForm {
    pub(super) username: String,
    pub(super) password: String,
    pub(super) role: String,
    pub(super) seller_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct WebUserDeleteForm {
    pub(super) id: String,
}

#[derive(Deserialize)]
pub(super) struct WebUserPasswordForm {
    pub(super) id: String,
    pub(super) password: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn webusers_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    let role = if Role::from_role_str(&f.role) == Role::Owner {
        "owner"
    } else {
        "seller"
    };
    // E13: empty seller_id stays None (platform-level) — the select's first
    // option submits "" and the parse here already maps that to None.
    let seller_id = f
        .seller_id
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok());
    let res = state
        .db
        .create_web_account(&f.username, &f.password, role, seller_id)
        .await;
    // E11: a duplicate username used to vanish (`let _ =`) — the honest
    // failure renders now. Other Db failures get their own key.
    let dest = match res {
        Ok(()) => format!("/webusers?created=1&u={}", qenc(f.username.trim())),
        Err(e) if e.contains("UNIQUE") => "/webusers?error=taken".to_string(),
        Err(_) => "/webusers?error=create".to_string(),
    };
    Redirect::to(&state.url(&dest)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn webusers_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        state.db.delete_web_account(id);
        return Redirect::to(&state.url("/webusers?deleted=1")).into_response();
    }
    Redirect::to(&state.url("/webusers")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn webusers_password(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserPasswordForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        // Honest failure (E11 sibling): the reset can fail (the
        // deleted-account edge) — never banner success for a swallowed Db
        // error.
        return match state.db.update_web_password_by_id(id, &f.password).await {
            Ok(()) => Redirect::to(&state.url("/webusers?password=1")).into_response(),
            Err(_) => Redirect::to(&state.url("/webusers?error=password")).into_response(),
        };
    }
    Redirect::to(&state.url("/webusers")).into_response()
}
