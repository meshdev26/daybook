use super::*;
// ---- Personal identity (user-identity Phase 1) -------------------------------
//
// Console surfaces for the owner's personal Telegram account, per
// design-system/daybook/IDENTITY_UX_SPEC.md and
// IDENTITY_MONITOR_UX_SPEC.md. Every state question goes to the identity
// actor per request (`IdentityHandle::status`); the connect wizard is
// stateless — the actor's state machine IS the wizard state.

use crate::identity::{IdentityError, IdentityState, IdentityStatus};

/// Process-wide snapshot of the identity subsystem, refreshed before every
/// request by [`identity_status_refresh`] and read synchronously by
/// `html_page` (the shell fn is sync and cannot await the actor). There is
/// exactly one identity per process — the owner's personal account — so a
/// single slot is exact. The unlock-result flash rides along because the
/// shell, not the page handler, renders the unlock feedback on whichever
/// console page the unlock POST returned to (IDENTITY_UX_SPEC §4.2). The
/// flash itself is a one-shot cookie (F7), never a query flag: a crafted URL
/// must not be able to flash state at the next request.
#[derive(Clone)]
pub(super) struct IdentitySnapshot {
    pub(super) status: IdentityStatus,
    /// The request carried `identity_flash=unlock_failed` (unlock POST
    /// failure flash): the banner re-renders with its `role="alert"` line.
    pub(super) unlock_failed: bool,
    /// The request carried `identity_flash=unlocked` (unlock POST success
    /// flash).
    pub(super) unlocked: bool,
}

pub(super) static IDENTITY_SNAPSHOT: StdMutex<IdentitySnapshot> = StdMutex::new(IdentitySnapshot {
    status: IdentityStatus {
        state: IdentityState::Disconnected,
        detail: String::new(),
        persisted: false,
    },
    unlock_failed: false,
    unlocked: false,
});

pub(super) fn identity_snapshot() -> IdentitySnapshot {
    IDENTITY_SNAPSHOT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Refresh the identity snapshot before each request (see `router`). Asset
/// paths are skipped — nothing there renders the shell.
///
/// The unlock flash (F7) is a one-shot cookie: the unlock POST sets
/// `identity_flash` (60s, HttpOnly, SameSite=Strict), this middleware reads
/// it into the snapshot, and the response that renders from it clears the
/// cookie in the same response (Set-Cookie Max-Age=0, appended so any
/// Set-Cookie the handler itself set survives). A plain GET sets nothing,
/// and query strings can no longer flash unlock state at the next owner.
pub(super) async fn identity_status_refresh(
    State(state): State<WebState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let flash = if !path.starts_with("/assets") && !path.starts_with("/static") {
        let status = state.identity.status().await;
        let carried = get_identity_flash_cookie(req.headers());
        let mut snap = IDENTITY_SNAPSHOT.lock().unwrap_or_else(|p| p.into_inner());
        snap.status = status;
        snap.unlock_failed = carried.as_deref() == Some("unlock_failed");
        snap.unlocked = carried.as_deref() == Some("unlocked");
        carried
    } else {
        None
    };
    let mut res = next.run(req).await;
    if flash.is_some() {
        // One-shot: the response that just consumed the flash expires the
        // cookie. HeaderValue::from_str cannot fail on these constant forms.
        if let Ok(v) = axum::http::HeaderValue::from_str(&clear_identity_flash_cookie(&state)) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

/// The standing unlock banner, above the measure on every console page while
/// the identity is locked (IDENTITY_UX_SPEC §4). Pure fn of the snapshot so
/// tests can render it without a live actor.
pub(super) fn identity_unlock_banner(base: &str, snap: &IdentitySnapshot) -> Option<String> {
    if snap.status.state != IdentityState::Locked {
        return None;
    }
    // Failure feedback renders inside the banner; the passphrase field is
    // never repopulated (spec: the field is cleared, nothing is stored).
    let error_span = if snap.unlock_failed {
        r#"<span role="alert">That passphrase did not unlock it. </span>"#
    } else {
        ""
    };
    Some(format!(
        r#"<div class="alert alert-warn unlock-banner" tabindex="-1">
<b>Personal identity locked.</b>
{error_span}Posting, replying and monitoring as you are paused until it's unlocked.
<form method="post" action="{base}/identity/unlock" class="inline-form alert-form">
<label for="unlock-passphrase" class="sr-only">Unlock passphrase</label>
<input type="password" id="unlock-passphrase" name="passphrase" autocomplete="off" required placeholder="Unlock passphrase">
<button type="submit" class="btn btn-sm">Unlock</button>
</form>
</div>"#
    ))
}

/// The pinned identity status line under the bot selector
/// (IDENTITY_UX_SPEC §1.3): owner-only, a fact line — word + badge, never
/// color alone, never merged with bot scope. Absent for everyone else.
pub(super) fn identity_status_line(
    base: &str,
    session: Option<&Session>,
    status: &IdentityStatus,
) -> String {
    let Some(s) = session else {
        return String::new();
    };
    if !Role::from_role_str(&s.role).has_level(Role::Owner) {
        return String::new();
    }
    let shell = |inner: &str| {
        format!(
            r#"<div class="bot-scope bot-scope-pinned" aria-label="Personal identity"><span class="help">Identity</span>{inner}</div>"#
        )
    };
    match status.state {
        IdentityState::Ready => shell(r#"<span class="badge badge-success">connected</span>"#),
        IdentityState::Locked => shell(r#"<span class="badge badge-warning">locked</span>"#),
        IdentityState::Disabled => shell(r#"<span class="badge badge-danger">disabled</span>"#),
        // Not set up or mid-wizard: a quiet link, no button chrome.
        _ => shell(&format!(r#"<a href="{base}/identity">Set up identity</a>"#)),
    }
}

/// (badge class, word) for the /identity status panel. Empty class renders
/// the bare word — plain text is the honest state for "nothing exists yet".
pub(super) fn identity_state_word(state: IdentityState) -> (&'static str, &'static str) {
    match state {
        IdentityState::Ready => ("badge-success", "connected"),
        IdentityState::Locked => ("badge-warning", "locked"),
        IdentityState::Disabled => ("badge-danger", "disabled"),
        IdentityState::AwaitingCode | IdentityState::AwaitingPassword => {
            ("badge-warning", "connecting")
        }
        IdentityState::Configured => ("", "not connected"),
        IdentityState::NotConfigured => ("", "not set up"),
        IdentityState::Disconnected => ("", "not running"),
    }
}

/// Honest, typed copy for every `IdentityError` (IDENTITY_UX_SPEC §3.8):
/// attempts-remaining is stated only when Telegram reports one (it never
/// does through this taxonomy, so the count is never invented), FloodWait
/// gets an absolute UTC stamp, and raw transport/internal strings are never
/// rendered.
pub(super) fn identity_error_copy(e: &IdentityError) -> String {
    match e {
        IdentityError::NotConfigured => {
            "Personal identity is not configured — save your API credentials in the wizard first."
                .to_string()
        }
        IdentityError::NotConnected => {
            "The personal identity subsystem is not running on this install. Nothing was sent to Telegram."
                .to_string()
        }
        IdentityError::Locked => {
            "That action needs an unlocked identity — unlock it in the banner above.".to_string()
        }
        IdentityError::NotAuthorized => "The personal identity is not signed in.".to_string(),
        IdentityError::WrongPassphrase => "That passphrase did not unlock it.".to_string(),
        IdentityError::InvalidCode => {
            "Telegram rejected that code. Try again — repeated failures trigger a temporary wait."
                .to_string()
        }
        IdentityError::InvalidPassword => {
            "Telegram rejected the 2FA password. It was not stored. Try again — repeated failures trigger a temporary wait."
                .to_string()
        }
        IdentityError::PasswordRequired => {
            "Telegram still requires the 2FA password to finish signing in.".to_string()
        }
        IdentityError::SignUpRequired => {
            "Telegram requires this account to sign up with an official client first; this console cannot register accounts."
                .to_string()
        }
        IdentityError::RateLimited(wait) => {
            format!("Console read limit reached — try again in {wait} seconds.")
        }
        IdentityError::FloodWait(secs) => {
            let secs_i = i64::try_from(*secs).unwrap_or(i64::MAX);
            let at = (chrono::Utc::now() + chrono::Duration::seconds(secs_i))
                .format("%Y-%m-%d %H:%M UTC");
            let mins = secs.div_ceil(60).max(1);
            format!(
                "Telegram is limiting sign-ins right now. Try again after {at} (in {mins} minutes)."
            )
        }
        IdentityError::AuthKeyDuplicated => {
            "Telegram refused this session: this api_id / api_hash pair is already authorized elsewhere. <b>Nothing was connected</b> — nothing was written."
                .to_string()
        }
        IdentityError::VersionMismatch { .. } => {
            "The stored session was written by a different app version — re-pair the personal identity."
                .to_string()
        }
        IdentityError::PeerNotFound(_) => "That channel isn't available to your account.".to_string(),
        IdentityError::ChatNotAllowed(_) => {
            "That chat isn't one of this console's explicitly configured channels — add it on the Channels page first."
                .to_string()
        }
        IdentityError::SurfaceHalted(what) => format!(
            "Personal identity {} are halted after repeated Telegram flood control — reset the surface or restart the app to continue.",
            esc(what)
        ),
        IdentityError::InvalidCursor => {
            "That page of history is outside the supported range — go back to the newest messages."
                .to_string()
        }
        IdentityError::InvalidState(what) => {
            format!("That step isn't available right now — the identity is {}.", esc(what))
        }
        IdentityError::Transport(_) => {
            "A Telegram connection problem occurred — try again shortly.".to_string()
        }
        IdentityError::Internal(_) => {
            "Something went wrong on the console side — try again.".to_string()
        }
    }
}

/// Error alert that carries focus on render (tabindex="-1", the
/// SETUP_MCP §5.3 idiom; APP_JS moves focus to `[data-focus-on-load]`).
/// `inner` is trusted markup — call [`identity_error_html`] for typed errors
/// and escape any dynamic text before it reaches here.
pub(super) fn focused_error_alert(inner: &str) -> String {
    format!(
        r#"<div class="alert alert-error" role="alert" tabindex="-1" data-focus-on-load>{inner}</div>"#
    )
}

pub(super) fn identity_error_html(e: &IdentityError) -> String {
    focused_error_alert(&identity_error_copy(e))
}

/// PRG redirect that also sets one cookie (the one-shot identity flash, F7).
pub(super) fn redirect_with_cookie(path: &str, cookie: String) -> Response {
    ([(header::SET_COOKIE, cookie)], Redirect::to(path)).into_response()
}

/// One-shot flash cookie for the unlock POST result (F7): the unlock handler
/// sets it on its PRG redirect, [`identity_status_refresh`] renders from it
/// on the next request and clears it in that same response. 60s — one
/// redirect hop, tolerant of a slow browser; HttpOnly + SameSite=Strict +
/// Path mirror the `mfa_pending` cookie idiom (auth.rs). Never set from a
/// query string: a URL must not be able to flash identity state at the next
/// viewer.
pub(super) fn identity_flash_cookie(state: &WebState, kind: &str) -> String {
    if secure_cookies() {
        format!(
            "identity_flash={kind}; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=60",
            state.cookie_path()
        )
    } else {
        format!(
            "identity_flash={kind}; Path={}; HttpOnly; SameSite=Strict; Max-Age=60",
            state.cookie_path()
        )
    }
}

/// The clearing form of [`identity_flash_cookie`], set on the response that
/// just rendered the flash.
pub(super) fn clear_identity_flash_cookie(state: &WebState) -> String {
    if secure_cookies() {
        format!(
            "identity_flash=; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=0",
            state.cookie_path()
        )
    } else {
        format!(
            "identity_flash=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
            state.cookie_path()
        )
    }
}

pub(super) fn get_identity_flash_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';').find_map(|p| {
                p.trim()
                    .strip_prefix("identity_flash=")
                    .map(|v| v.to_string())
            })
        })
}

#[derive(Deserialize)]
pub(super) struct IdentityQuery {
    pub(super) connected: Option<String>,
    pub(super) posting: Option<String>,
    pub(super) disconnected: Option<String>,
    pub(super) error: Option<String>,
}

/// Flash banners on `/identity` (IDENTITY_UX_SPEC §2.2), page-local PRG
/// flags. The unlock result flash is NOT one of these — it rides the
/// one-shot `identity_flash` cookie (F7) and renders globally in `html_page`.
pub(super) fn identity_flash(q: &IdentityQuery) -> String {
    let mut out = String::new();
    if q.connected.as_deref() == Some("1") {
        out.push_str(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Personal identity connected.</div>"#,
        );
    }
    if q.posting.as_deref() == Some("1") {
        out.push_str(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Posting as your personal account is enabled.</div>"#,
        );
    }
    if q.disconnected.as_deref() == Some("1") {
        out.push_str(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Personal identity disconnected.</div>"#,
        );
    }
    match q.error.as_deref() {
        Some("locked") => out.push_str(&focused_error_alert(
            "That action needs an unlocked identity — unlock it in the banner above.",
        )),
        Some("posting") => out.push_str(&focused_error_alert(
            "You must accept the risks before posting as your personal account. Nothing changed.",
        )),
        _ => {}
    }
    out
}

/// Owner session gate shared by every /identity*, /monitor route: the rail
/// never renders the group for other roles, and the routes re-derive the
/// role server-side (the buttons are affordances, not the gate).
// Response is a large enum; boxing it would ripple through every console
// route's early-return idiom for no real gain.
#[allow(clippy::result_large_err)]
pub(super) fn identity_owner_session(
    state: &WebState,
    headers: &axum::http::HeaderMap,
    page: &str,
) -> Result<Session, Response> {
    let Some(session) = session_from_headers(state, headers) else {
        return Err(Redirect::to(&state.url("/login")).into_response());
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return Err(forbidden_page(state, page, page));
    }
    Ok(session)
}

#[tracing::instrument(skip_all)]
pub(super) async fn identity_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<IdentityQuery>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Identity") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let status = state.identity.status().await;
    let gate = state.db.get_config("identity_writes_enabled");
    let gate_at = state.db.get_config("identity_writes_enabled_at");
    let mut body = format!(
        r#"<h1>Identity <span class="sub">your personal Telegram account — the one that posts and replies as you</span></h1>{}"#,
        identity_flash(&q)
    );
    match status.state {
        // No identity exists yet: the page is one honest panel with the
        // single primary action. The Enable-posting panel cannot exist
        // before an identity does (IDENTITY_UX_SPEC §2.5).
        IdentityState::NotConfigured | IdentityState::Disconnected => {
            body.push_str(&format!(
                r#"<article class="panel"><p class="empty"><b>No personal identity connected.</b> Connect one to post and reply as yourself.</p>
<p><a class="btn" href="{}">Connect personal identity</a></p></article>"#,
                state.url("/identity/wizard")
            ));
        }
        _ => {
            body.push_str(&identity_status_panel(&state, &status));
            body.push_str(&identity_posting_panel(
                &state,
                &status,
                gate.as_deref(),
                gate_at.as_deref(),
            ));
        }
    }
    Html(html_page(&state, Some(&session), "Identity", &body)).into_response()
}

pub(super) fn identity_status_panel(state: &WebState, status: &IdentityStatus) -> String {
    let (cls, word) = identity_state_word(status.state);
    let badge = if cls.is_empty() {
        word.to_string()
    } else {
        format!(r#"<span class="badge {cls}">{word}</span>"#)
    };
    let persisted = if status.persisted {
        "stored — encrypted under your unlock passphrase"
    } else {
        "not stored yet"
    };
    format!(
        r#"<article class="panel"><h3>Status</h3>
<dl class="kv">
<dt>Status</dt><dd>{badge}</dd>
<dt>Detail</dt><dd class="mono">{}</dd>
<dt>Session</dt><dd>{persisted}</dd>
</dl>
{}</article>"#,
        esc(&status.detail),
        identity_actions(state, status)
    )
}

/// confirm() copy for the destructive wipe (F5): it names the consequence,
/// and must stay free of single quotes (it renders inside
/// `confirm('...')`).
pub(super) const IDENTITY_WIPE_CONFIRM: &str = "Wipe stored identity? This removes the stored personal-identity session on this machine; you will need to pair again.";

/// The destructive wipe affordance (F5): an inline form with a confirm()
/// naming the consequence. Offered on the Disabled panel (the only way back
/// to a connectable state) and on Ready as the deliberate escape hatch.
pub(super) fn identity_wipe_form(state: &WebState) -> String {
    format!(
        r#"<form method="post" action="{}" class="inline-form"><button type="submit" class="btn btn-danger" onclick="return confirm('{}')">Wipe stored identity</button></form>"#,
        state.url("/identity/wipe"),
        IDENTITY_WIPE_CONFIRM
    )
}

/// Per-state actions on /identity. While locked there is deliberately no
/// second unlock form — the banner above is the one unlock affordance
/// (IDENTITY_UX_SPEC §4.1).
pub(super) fn identity_actions(state: &WebState, status: &IdentityStatus) -> String {
    let wizard = state.url("/identity/wizard");
    match status.state {
        IdentityState::Ready => {
            let confirm = if status.persisted {
                "Disconnect your personal identity? Posting, replying and monitoring pause until you unlock again."
            } else {
                "Disconnect now? This sign-in is not stored yet — you would have to start the connection over."
            };
            let mut s = String::new();
            if !status.persisted {
                s.push_str(&format!(
                    r#"<p><a class="btn" href="{wizard}">Finish — set the unlock passphrase</a></p>"#
                ));
            }
            s.push_str(&format!(
                r#"<form method="post" action="{}" class="inline-form"><button type="submit" class="btn btn-danger" onclick="return confirm('{}')">Disconnect</button></form>"#,
                state.url("/identity/disconnect"),
                confirm
            ));
            s.push_str(&identity_wipe_form(state));
            s
        }
        IdentityState::Locked => {
            r#"<p class="help">Locked — use the Unlock form in the banner above.</p>"#.to_string()
        }
        IdentityState::Disabled => format!(
            r#"<p class="help">Telegram reported this session's authorization key is used by another client. The stored session must be removed and the identity paired again before anything reconnects.</p>
<p><a class="btn" href="{wizard}">Review connection</a></p>
{}"#,
            identity_wipe_form(state)
        ),
        IdentityState::NotConfigured | IdentityState::Disconnected => {
            format!(r#"<p><a class="btn" href="{wizard}">Connect personal identity</a></p>"#)
        }
        IdentityState::Configured
        | IdentityState::AwaitingCode
        | IdentityState::AwaitingPassword => {
            format!(r#"<p><a class="btn" href="{wizard}">Continue connecting</a></p>"#)
        }
    }
}

/// The Enable-posting gate (IDENTITY_UX_SPEC §5): the second and last ToS
/// surface. Once accepted it collapses to a one-line record.
pub(super) fn identity_posting_panel(
    state: &WebState,
    status: &IdentityStatus,
    gate: Option<&str>,
    gate_at: Option<&str>,
) -> String {
    if let Some(_accepted) = gate {
        let when = gate_at.unwrap_or("an earlier date");
        return format!(
            r#"<article class="panel"><h3>Enable posting</h3>
<p><span class="badge badge-success">posting enabled</span></p>
<p class="help">Accepted {}. Removing the gate is a v2 decision — see follow-ups.</p></article>"#,
            esc(when)
        );
    }
    // The panel cannot exist before an identity does; while locked the POST
    // is rejected server-side (?error=locked), the panel just explains why.
    let exists = matches!(status.state, IdentityState::Ready | IdentityState::Locked);
    if !exists {
        return String::new();
    }
    let locked_help = if status.state == IdentityState::Locked {
        r#"<p class="help">The identity is locked — unlock it in the banner above before enabling posting.</p>"#
    } else {
        ""
    };
    format!(
        r#"<article class="panel"><h3>Enable posting</h3>
<p>Posting as your personal account sends channel posts from <b>you</b>, not the bot. Telegram may limit or suspend accounts that automate actions, and members see the posts as yours. Anyone with console access can post as you.</p>
<form method="post" action="{}">
<div><input type="checkbox" id="posting-accept" name="accept" value="1" required aria-required="true"> <label for="posting-accept">I understand the risks</label></div>
<div><button type="submit" class="btn">Enable posting</button></div>
</form>
{locked_help}
</article>"#,
        state.url("/identity/enable-writes"),
    )
}

#[derive(Deserialize)]
pub(super) struct IdentityEnableWritesForm {
    pub(super) accept: Option<String>,
}

/// POST /identity/enable-writes — the posting gate. Gated by the risk
/// checkbox, by the `identity_tos_accepted` config flag (F10: the wizard
/// sets it in step 1 before login, so no behavior change today — this
/// closes the future-path bypass where a ToS-less identity hits Ready by
/// some other route), and by the unlock state (a uniform rule: everything
/// identity-consuming is rejected while locked, IDENTITY_UX_SPEC §4.2).
#[tracing::instrument(skip_all)]
pub(super) async fn identity_enable_writes(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityEnableWritesForm>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Identity") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if f.accept.as_deref().filter(|s| !s.is_empty()).is_none() {
        return Redirect::to(&state.url("/identity?error=posting")).into_response();
    }
    // F10 defense in depth: the checkbox alone is not the ToS — the flag the
    // wizard's step 1 wrote must exist too. Without it, point at that step.
    if state.db.get_config("identity_tos_accepted").is_none() {
        let body = format!(
            "{}<p><a class=\"btn\" href=\"{}\">Open the connect wizard</a></p>",
            focused_error_alert(
                "Enable posting needs the connect wizard's step 1 — the risk acceptance. Nothing was changed.",
            ),
            state.url("/identity/wizard")
        );
        return Html(html_page(&state, Some(&session), "Identity", &body)).into_response();
    }
    let status = state.identity.status().await;
    match status.state {
        IdentityState::Locked => {
            return Redirect::to(&state.url("/identity?error=locked")).into_response();
        }
        IdentityState::Ready => {}
        _ => return Redirect::to(&state.url("/identity")).into_response(),
    }
    state.db.set_config("identity_writes_enabled", "1");
    state.db.set_config(
        "identity_writes_enabled_at",
        &chrono::Utc::now().to_rfc3339(),
    );
    state.db.log_action(
        session.account_id,
        &session.role,
        "identity_posting_enabled",
        "identity",
        "0",
        None,
    );
    Redirect::to(&state.url("/identity?posting=1")).into_response()
}

#[derive(Deserialize)]
pub(super) struct IdentityUnlockForm {
    pub(super) passphrase: Option<String>,
}

/// Server-side upper bound for the unlock passphrase and the wizard's
/// set-passphrase step (F9): an honest refusal, no actor call. The stored
/// passphrase policy is `minlength="8"` client-side; 128 is a generous
/// ceiling that keeps absurd inputs from reaching the AEAD open.
pub(super) const PASSPHRASE_MAX_CHARS: usize = 128;

/// POST /identity/unlock — the standing banner's submit, available on every
/// console page while locked. Wrong-passphrase attempts share the login
/// throttle (5 / 900s, IDENTITY_UX_SPEC §7.6; keys are namespaced — see
/// `login_submit` — so the shared map cannot be poisoned across flows); the
/// passphrase itself is never logged, hashed, or stored — unlock IS the AEAD
/// open attempt. The result rides a one-shot flash cookie (F7), never a
/// query flag.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_unlock(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<IdentityUnlockForm>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Identity") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let back = referer_path(&state, &headers).unwrap_or_else(|| state.url("/identity"));
    let passphrase = f.passphrase.unwrap_or_default();
    // Upper bound first (F9): an honest alert, and neither an actor call nor
    // a consumed throttle attempt.
    if passphrase.chars().count() > PASSPHRASE_MAX_CHARS {
        let body = format!(
            r#"<div class="alert alert-error" role="alert">Passphrase must be at most 128 characters.</div><p><a href="{}">Back</a></p>"#,
            esc(&back)
        );
        return (
            StatusCode::BAD_REQUEST,
            Html(html_page(&state, Some(&session), "Identity", &body)),
        )
            .into_response();
    }
    let throttle_key = format!("identity-unlock:{}", session.account_id);
    if login_throttle(&state.login_attempts, &throttle_key, 5, 900) {
        // The login page's exact throttle string (IDENTITY_UX_SPEC §4.2).
        let body = format!(
            r#"<div class="alert alert-error" role="alert">Too many attempts. Try again in a few minutes.</div><p><a href="{}">Back</a></p>"#,
            esc(&back)
        );
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Html(html_page(&state, Some(&session), "Identity", &body)),
        )
            .into_response();
    }
    match state.identity.unlock(&passphrase).await {
        Ok(()) => {
            login_ok(&state.login_attempts, &throttle_key);
            state.db.log_action(
                session.account_id,
                &session.role,
                "identity_unlock",
                "identity",
                "0",
                None,
            );
            redirect_with_cookie(&back, identity_flash_cookie(&state, "unlocked"))
        }
        Err(IdentityError::WrongPassphrase) => {
            // Failure is logged as an attempt, never with the passphrase.
            state.db.log_action(
                session.account_id,
                &session.role,
                "identity_unlock_failed",
                "identity",
                "0",
                None,
            );
            redirect_with_cookie(&back, identity_flash_cookie(&state, "unlock_failed"))
        }
        Err(_) => Redirect::to(&state.url("/identity?error=locked")).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct IdentityNoFieldsForm {}

/// POST /identity/disconnect — drops the live client (with a persisted
/// session the subsystem returns to Locked; without one to Configured).
#[tracing::instrument(skip_all)]
pub(super) async fn identity_disconnect(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(_f): Form<IdentityNoFieldsForm>,
) -> Response {
    if let Err(resp) = identity_owner_session(&state, &headers, "Identity") {
        return resp;
    }
    let status = state.identity.status().await;
    if status.state != IdentityState::Ready {
        return Redirect::to(&state.url("/identity")).into_response();
    }
    let flash = if state.identity.disconnect().await.is_ok() {
        "/identity?disconnected=1"
    } else {
        "/identity"
    };
    Redirect::to(&state.url(flash)).into_response()
}

/// POST /identity/wipe — destructive recovery (F5): removes the stored
/// personal-identity session blob and resets the subsystem, so /identity
/// renders the Connect path again. Owner-gated like every identity route;
/// audited as `identity_wiped` (the owner's destructive request is on the
/// record even where the actor refuses to apply it — it fails closed);
/// always PRGs to /identity, whose state panel then shows the truth.
#[tracing::instrument(skip_all)]
pub(super) async fn identity_wipe(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(_f): Form<IdentityNoFieldsForm>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Identity") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let _ = state.identity.wipe().await;
    state.db.log_action(
        session.account_id,
        &session.role,
        "identity_wiped",
        "identity",
        "0",
        None,
    );
    Redirect::to(&state.url("/identity")).into_response()
}
