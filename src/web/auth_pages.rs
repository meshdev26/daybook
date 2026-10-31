use super::*;
#[tracing::instrument(skip_all)]
pub(super) async fn index(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if !setup_done(&state) {
        return Redirect::to(&state.url("/setup")).into_response();
    }
    if session_from_headers(&state, &headers).is_some() {
        Redirect::to(&state.url("/dashboard")).into_response()
    } else {
        Redirect::to(&state.url("/login")).into_response()
    }
}

/// Query shared by the login GET and POST: a `?code=` deep link (printed to
/// the server console once at startup) is how a remote operator reaches the
/// claim form without being on the machine.
#[derive(Deserialize)]
pub(super) struct ClaimQuery {
    pub(super) code: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct LoginForm {
    pub(super) username: String,
    pub(super) password: String,
    /// Claim mode only: the confirmation retype. Server-side checked — the
    /// browser's minlength is a courtesy, never the gate.
    pub(super) confirm_password: Option<String>,
    /// Claim mode only: the one-time code, either as a form field or via
    /// `?code=` (see [`ClaimQuery`]). Never rendered back on failure.
    pub(super) claim_code: Option<String>,
}

/// The sign-in form. `alert` (already-escaped or static text) is shown above
/// it — used by the GET page and both failure branches so the three surfaces
/// stay one form, one vocabulary.
pub(super) fn sign_in_form_html(state: &WebState, alert: Option<&str>) -> String {
    let alert = alert
        .map(|a| format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, a))
        .unwrap_or_default();
    format!(
        r#"{alert}<h1>Sign in</h1>
<p class="help">The operator console for your Telegram shop.</p>
<form method="post" action="{}">
<label for="username">Username</label><input type="text" id="username" name="username" required autocomplete="username">
<label for="login-password">Password</label>
<span class="secret-field"><input type="password" id="login-password" name="password" required autocomplete="current-password">{toggle}</span>
<button type="submit">Sign in</button>
</form>"#,
        state.url("/login"),
        toggle = SECRET_TOGGLE,
    )
}

/// The claim form: the front door while no owner account exists. `provided`
/// is the caller-SUPPLIED code (from `?code=` or a previous attempt) carried
/// in a hidden field so a remote operator who mistypes their password keeps
/// their code; the server's real code is never echoed anywhere.
pub(super) fn claim_form_html(
    state: &WebState,
    alert: Option<&str>,
    provided: Option<&str>,
) -> String {
    let alert = alert
        .map(|a| format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, a))
        .unwrap_or_default();
    let hidden = provided
        .map(|c| {
            format!(
                r#"<input type="hidden" name="claim_code" value="{}">"#,
                esc(c)
            )
        })
        .unwrap_or_default();
    format!(
        r#"{alert}<h1>Claim the console</h1>
<p class="help">No dashboard account exists yet. The account you create here becomes the owner — it manages settings, channels and sellers. This offer ends once an owner exists.</p>
<form method="post" action="{}">
{hidden}
<label for="claim-username">Username</label><input type="text" id="claim-username" name="username" required autocomplete="username">
<label for="claim-password">Password</label>
<span class="secret-field"><input type="password" id="claim-password" name="password" required minlength="6" autocomplete="new-password">{toggle}</span>
<label for="claim-confirm-password">Confirm password</label>
<span class="secret-field"><input type="password" id="claim-confirm-password" name="confirm_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters, typed twice.</span>
<button type="submit">Claim the console</button>
</form>"#,
        state.url("/login"),
        hidden = hidden,
        toggle = SECRET_TOGGLE,
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn login_page(
    State(state): State<WebState>,
    Query(q): Query<ClaimQuery>,
) -> impl IntoResponse {
    if !setup_done(&state) {
        return Redirect::to(&state.url("/setup")).into_response();
    }
    if !state.db.owner_account_exists() {
        // Claim mode: the front door IS the claim form.
        return Html(html_page_bare(
            &state.base_path,
            "Claim",
            &claim_form_html(&state, None, q.code.as_deref()),
        ))
        .into_response();
    }
    Html(html_page_bare(
        &state.base_path,
        "Login",
        &sign_in_form_html(&state, None),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn login_submit(
    State(state): State<WebState>,
    peer: PeerIp,
    Query(q): Query<ClaimQuery>,
    Form(f): Form<LoginForm>,
) -> impl IntoResponse {
    // Claim mode: with no owner account, POST /login IS the claim submission.
    // Once an owner exists this branch can never run again (claim mode never
    // returns), so normal sign-in below stays the steady state.
    if !state.db.owner_account_exists() {
        return claim_submit(&state, &peer, &q, &f).await;
    }
    // Rate limit: 5 failed attempts per username per 15 minutes
    // (a successful login clears the counter via `login_ok`). Keys are
    // namespaced (`login:{username}`) because this map is shared with other
    // flows that key it differently (identity unlock:
    // `identity-unlock:{account_id}`, MFA: `mfa:{token}`) — a raw username
    // could collide with one of those namespaces, so 5 failed logins AS the
    // username "identity-unlock:<owner id>" would lock that owner's unlock
    // form for 900s, repeatable by an unauthenticated attacker. Usernames
    // that themselves start with `login:` become ambiguous with this
    // namespace — acceptable: usernames are admin-created.
    let throttle_key = format!("login:{}", f.username);
    if login_throttle(&state.login_attempts, &throttle_key, 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "Login",
                &format!(
                    r##"<div class="alert alert-error" role="alert">Too many attempts. Try again in a few minutes.</div>
<p><a class="btn btn-secondary" href="{}">Back to sign in</a></p>"##,
                    state.url("/login")
                ),
            )),
        )
            .into_response();
    }
    if let Some((account_id, role, seller_id)) =
        state.db.verify_web_password(&f.username, &f.password).await
    {
        login_ok(&state.login_attempts, &throttle_key);
        // MFA policy stays ENFORCED unless an explicit opt-out row exists:
        // no console surface writes `mfa_disabled_{id}` yet, so a fresh
        // install (no config row) still forces enrollment for privileged
        // accounts. Defaulting the missing row to "disabled" here would turn
        // forced enrollment off for everyone — the key is only ever a
        // deliberate, per-account opt-out.
        let mfa_disabled = state
            .db
            .get_config(&format!("mfa_disabled_{}", account_id))
            .map(|v| v == "1")
            .unwrap_or(false);
        return login_complete(&state, account_id, role, seller_id, mfa_disabled).await;
    } else {
        // The failure was already recorded by `login_throttle` above.
        Html(html_page_bare(
            &state.base_path,
            "Login",
            &sign_in_form_html(
                &state,
                Some("Invalid credentials. Check the username and password, then try again."),
            ),
        ))
        .into_response()
    }
}

async fn login_complete(
    state: &WebState,
    account_id: i64,
    role: String,
    seller_id: Option<i64>,
    mfa_disabled: bool,
) -> Response {
    let (_totp_secret, totp_enabled, _last) = state.db.get_totp_state(account_id);
    if !mfa_disabled && totp_enabled {
        let pending_raw = crate::util::gen_token(48);
        let pending_hash = sha256_hex(&pending_raw);
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        let _ = state.db.create_pending_mfa(
            pending_hash,
            account_id,
            "verify",
            None::<String>,
            expires,
        );
        return (
            [(header::SET_COOKIE, pending_cookie(state, &pending_raw))],
            Redirect::to(&state.url("/mfa/verify")),
        )
            .into_response();
    }
    let privileged = role == "owner" || role == "admin";
    if !mfa_disabled && privileged {
        let mut buf = [0u8; 20];
        let _ = getrandom::fill(&mut buf);
        let secret = crate::util::base32_encode(&buf);
        let pending_raw = crate::util::gen_token(48);
        let pending_hash = sha256_hex(&pending_raw);
        let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        let _ =
            state
                .db
                .create_pending_mfa(pending_hash, account_id, "enroll", Some(secret), expires);
        return (
            [(header::SET_COOKIE, pending_cookie(state, &pending_raw))],
            Redirect::to(&state.url("/mfa/enroll")),
        )
            .into_response();
    }
    let token = state.db.create_web_session(account_id, &role, seller_id, 7);
    (
        [(header::SET_COOKIE, session_cookie(state, &token))],
        Redirect::to(&state.url("/dashboard")),
    )
        .into_response()
}

/// Anti-takeover gate for the claim flow (PLAN v1.3.x). The one-time code is
/// waived only when the request provably arrives from the machine itself AND
/// the server is genuinely local-only — the desktop default bind is
/// `127.0.0.1`, where claiming with no code is the whole point. Behind a
/// reverse proxy on the same host (`WEB_BIND=0.0.0.0`) every remote browser
/// arrives with a loopback peer address, so the peer alone proves nothing:
/// `Config::web_bind()` must also be a loopback form ("127.0.0.1",
/// "localhost", "::1"). Anything else — a wildcard or LAN bind, or a remote
/// peer — requires the one-time claim code generated at startup, compared in
/// constant time; the code itself is never rendered back.
pub(super) fn claim_authorized(state: &WebState, peer: &PeerIp, provided: Option<&str>) -> bool {
    let bind = state.cfg.web_bind();
    let local_only = matches!(bind.trim(), "127.0.0.1" | "localhost" | "::1");
    if peer.is_loopback() && local_only {
        return true;
    }
    match (
        provided.map(str::trim).filter(|c| !c.is_empty()),
        state.db.claim_code(),
    ) {
        (Some(code), Some(expected)) => ct_eq(code, &expected),
        _ => false,
    }
}

/// Claim-mode POST /login: create the owner account and sign it in, exactly
/// like a successful sign-in. Every refusal renders the claim form again —
/// with the caller's own code (if any) preserved in the hidden field.
#[tracing::instrument(skip_all)]
pub(super) async fn claim_submit(
    state: &WebState,
    peer: &PeerIp,
    q: &ClaimQuery,
    f: &LoginForm,
) -> Response {
    // Same throttle as sign-in, same `login:` namespace (trimmed, the form
    // the account is created under): a claim endpoint that guesses passwords
    // at full speed is just a login form with worse hygiene.
    let throttle_key = format!("login:{}", f.username.trim());
    if login_throttle(&state.login_attempts, &throttle_key, 5, 900) {
        return claim_error(
            state,
            None,
            "Too many attempts. Try again in a few minutes.",
        );
    }
    // The gate runs before any account work: a remote request without a
    // valid code learns nothing except that the console is unclaimed —
    // which the public setup-skip flow already reveals.
    let provided = q.code.as_deref().or(f.claim_code.as_deref());
    if !claim_authorized(state, peer, provided) {
        return claim_error(
            state,
            provided,
            "Claiming requires the one-time code shown in the server console.",
        );
    }
    let uname = f.username.trim();
    let pw = f.password.as_str();
    if uname.is_empty() {
        return claim_error(state, provided, "Choose a username.");
    }
    if pw.chars().count() < 6 {
        return claim_error(state, provided, "Password must be at least 6 characters.");
    }
    if f.confirm_password.as_deref() != Some(pw) {
        return claim_error(state, provided, "Passwords do not match.");
    }
    if state.db.get_web_account_by_username(uname).is_some() {
        return claim_error(state, provided, "That username is taken.");
    }
    if let Err(e) = state.db.create_web_account(uname, pw, "owner", None).await {
        return claim_error(
            state,
            provided,
            &format!("Could not create the account: {}", esc(&e)),
        );
    }
    login_ok(&state.login_attempts, &throttle_key);
    // The freshly created account is the owner session; mirror login_submit's
    // cookie contract exactly (same lifetime, same flags).
    let (account_id, _u, _hash, _role, _seller) = state
        .db
        .get_web_account_by_username(uname)
        .expect("account was just created");
    let token = state.db.create_web_session(account_id, "owner", None, 7);
    (
        [(header::SET_COOKIE, session_cookie(state, &token))],
        Redirect::to(&state.url("/dashboard")),
    )
        .into_response()
}

pub(super) fn claim_error(state: &WebState, provided: Option<&str>, msg: &str) -> Response {
    Html(html_page_bare(
        &state.base_path,
        "Claim",
        &claim_form_html(state, Some(msg), provided),
    ))
    .into_response()
}

pub(super) fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(64);
    for b in bytes.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

/// The `session=<token>` cookie for a fresh login, shared by sign-in and
/// claim so both doors hand out identical cookies.
pub(super) fn session_cookie(state: &WebState, token: &str) -> String {
    if secure_cookies() {
        format!(
            "session={}; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=604800",
            token,
            state.cookie_path()
        )
    } else {
        format!(
            "session={}; Path={}; HttpOnly; SameSite=Strict; Max-Age=604800",
            token,
            state.cookie_path()
        )
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn logout(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_cookie(&headers) {
        state.db.delete_web_session(&token);
    }
    let cookie = if secure_cookies() {
        format!(
            "session=; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=0",
            state.cookie_path()
        )
    } else {
        format!(
            "session=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
            state.cookie_path()
        )
    };
    (
        [(header::SET_COOKIE, cookie)],
        Redirect::to(&state.url("/login")),
    )
        .into_response()
}
