use super::test_support::*;
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

fn tm(id: i64, reply_to: Option<i64>, sender: &str, text: &str) -> NormalizedMessage {
    NormalizedMessage {
        id,
        date: "2026-10-27T00:00:00+00:00".to_string(),
        sender: sender.to_string(),
        text: text.to_string(),
        reply_to,
    }
}

fn ready_status() -> crate::identity::IdentityStatus {
    crate::identity::IdentityStatus {
        state: crate::identity::IdentityState::Ready,
        detail: "connected".to_string(),
        persisted: true,
    }
}

#[tokio::test]
async fn identity_routes_are_owner_only() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 1).await;
    let app = router(state);
    // Sellers get 403 on every identity surface; the rail never
    // offered it, and the server refuses regardless.
    for uri in ["/identity", "/monitor", "/identity/wizard"] {
        let resp = app.clone().oneshot(authed_req(uri, &seller)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
    for (uri, body) in [
        ("/identity/unlock", "passphrase=x"),
        ("/identity/enable-writes", "accept=1"),
        ("/identity/wizard/accept", "accept=1"),
    ] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(uri, body, &seller))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
    }
    // Unauthenticated → 303 /login.
    let resp = app.clone().oneshot(get_req("/identity")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
    // Owner renders all three GETs.
    for uri in ["/identity", "/monitor", "/identity/wizard"] {
        let resp = app.clone().oneshot(authed_req(uri, &owner)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
    }
    // The destructive wipe (F5) obeys the same gates.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/identity/wipe", "", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = app.oneshot(post_req("/identity/wipe", "")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn rail_group_identity_owner_only() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 1).await;
    let app = router(state);
    let body = body_string(
        app.clone()
            .oneshot(authed_req("/dashboard", &owner))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        body.contains(r#"<section class="nav-group" data-nav-group="identity""#),
        "{body}"
    );
    assert!(body.contains(r#"href="/identity""#), "{body}");
    assert!(body.contains(r#"href="/monitor""#), "{body}");
    // Inbox is Phase 3 — must not be offered yet.
    assert!(!body.contains(r#"href="/inbox""#), "{body}");
    let seller_body = body_string(
        app.oneshot(authed_req("/dashboard", &seller))
            .await
            .unwrap(),
    )
    .await;
    assert!(!seller_body.contains(r#"<section class="nav-group" data-nav-group="identity""#));
}

#[tokio::test]
async fn unlock_banner_absent_when_not_locked_and_owner_line_renders() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    let body = body_string(app.oneshot(authed_req("/dashboard", &owner)).await.unwrap()).await;
    // Subsystem Disconnected: no banner, quiet "Set up identity" link.
    assert!(!body.contains("unlock-banner"), "{body}");
    assert!(body.contains("Set up identity"), "{body}");
}

#[test]
fn unlock_banner_renders_for_locked_snapshot() {
    let locked = IdentitySnapshot {
        status: crate::identity::IdentityStatus {
            state: crate::identity::IdentityState::Locked,
            detail: "session locked; unlock required".to_string(),
            persisted: true,
        },
        unlock_failed: false,
        unlocked: false,
    };
    let banner = identity_unlock_banner("", &locked).expect("locked renders banner");
    assert!(
        banner.contains(r#"class="alert alert-warn unlock-banner""#),
        "{banner}"
    );
    assert!(banner.contains(r#"tabindex="-1""#), "{banner}");
    assert!(banner.contains(r#"action="/identity/unlock""#), "{banner}");
    assert!(
        banner.contains(r#"autocomplete="off""#) && banner.contains("passphrase"),
        "{banner}"
    );
    assert!(banner.contains(r#"class="sr-only""#), "{banner}");
    assert!(
        !banner.contains("badge"),
        "banner must not carry status-line badges"
    );
    // Failure flash renders the role=alert line inside the banner.
    let failed = IdentitySnapshot {
        unlock_failed: true,
        ..locked.clone()
    };
    let banner = identity_unlock_banner("", &failed).unwrap();
    assert!(banner.contains(r#"role="alert""#), "{banner}");
    assert!(
        banner.contains("That passphrase did not unlock it."),
        "{banner}"
    );
    // Never for other states.
    let ready = IdentitySnapshot {
        status: ready_status(),
        unlock_failed: false,
        unlocked: false,
    };
    assert!(identity_unlock_banner("", &ready).is_none());
}

#[test]
fn identity_status_line_words_per_state() {
    let owner = Session {
        account_id: 1,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let seller = Session {
        account_id: 2,
        role: "seller".to_string(),
        seller_id: Some(7),
        selected_bot_id: None,
    };
    let st = |state: crate::identity::IdentityState| crate::identity::IdentityStatus {
        state,
        detail: String::new(),
        persisted: false,
    };
    use crate::identity::IdentityState as S;
    let line = identity_status_line("", Some(&owner), &st(S::Ready));
    assert!(
        line.contains(r#"badge badge-success">connected</span>"#),
        "{line}"
    );
    let line = identity_status_line("", Some(&owner), &st(S::Locked));
    assert!(
        line.contains(r#"badge badge-warning">locked</span>"#),
        "{line}"
    );
    let line = identity_status_line("", Some(&owner), &st(S::Disabled));
    assert!(
        line.contains(r#"badge badge-danger">disabled</span>"#),
        "{line}"
    );
    let line = identity_status_line("", Some(&owner), &st(S::NotConfigured));
    assert!(line.contains("Set up identity"), "{line}");
    // Seller/admin never see the line.
    assert!(identity_status_line("", Some(&seller), &st(S::Ready)).is_empty());
    assert!(identity_status_line("", None, &st(S::Ready)).is_empty());
}

#[tokio::test]
async fn wizard_renders_step_from_state_and_tos() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let app = router(state);
    // No ToS acceptance yet → step 1 regardless of Disconnected state.
    let body = body_string(
        app.clone()
            .oneshot(authed_req("/identity/wizard", &owner))
            .await
            .unwrap(),
    )
    .await;
    assert!(body.contains("Step 1 of 5"), "{body}");
    assert!(body.contains(r#"id="tos-accept""#), "{body}");
    assert!(body.contains(r#"aria-required="true""#), "{body}");
    // Accept → step 2 (credentials) with the my.telegram.org help.
    db.set_config("identity_tos_accepted", "1");
    let body = body_string(
        app.oneshot(authed_req("/identity/wizard", &owner))
            .await
            .unwrap(),
    )
    .await;
    assert!(body.contains("Step 2 of 5"), "{body}");
    assert!(body.contains("my.telegram.org"), "{body}");
    assert!(body.contains(r#"autocomplete="off""#), "{body}");
}

#[tokio::test]
async fn wizard_accept_requires_the_checkbox_and_changes_nothing_without_it() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let app = router(state);
    // Unchecked box reaching the server: honest refusal, no write.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/identity/wizard/accept", "", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("You must accept the risks before connecting. Nothing was sent to Telegram."),
        "{body}"
    );
    assert!(body.contains(r#"role="alert""#), "{body}");
    assert_eq!(db.get_config("identity_tos_accepted"), None);
    // Checked: PRG back to the wizard, acceptance recorded.
    let resp = app
        .oneshot(post_authed_req(
            "/identity/wizard/accept",
            "accept=1",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/identity/wizard");
    assert_eq!(db.get_config("identity_tos_accepted").as_deref(), Some("1"));
}

#[tokio::test]
async fn wizard_steps_fail_honestly_without_a_live_actor() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    state.db.set_config("identity_tos_accepted", "1");
    let app = router(state);
    // Phone step against an unconnected subsystem: honest typed error,
    // the phone preserved, no panic.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/wizard/phone",
            "phone=%2B491701234567",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
            body.contains(
                "The personal identity subsystem is not running on this install. Nothing was sent to Telegram."
            ),
            "{body}"
        );
    assert!(body.contains(r#"value="+491701234567""#), "{body}");
    // Code step: same honest surface.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/wizard/code",
            "code=12345",
            &owner,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("The personal identity subsystem is not running on this install."),
        "{body}"
    );
    // 2FA password step: same honest surface; nothing about the
    // password is ever echoed.
    let resp = app
        .oneshot(post_authed_req(
            "/identity/wizard/password",
            "password=hunter2",
            &owner,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("The personal identity subsystem is not running on this install."),
        "{body}"
    );
    assert!(!body.contains("hunter2"), "2FA password echoed back");
}

#[tokio::test]
async fn wizard_credentials_persist_env_and_set_restart_flag() {
    let (state, dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    // The ToS acceptance gates credential writes: nothing is saved
    // before step 1 exists.
    db.set_config("identity_tos_accepted", "1");
    let env_path = dir.path().join("identity-wizard.env");
    let _guard = ENV_LOCK.lock().await;
    set_env_file_var(&env_path);
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req(
            "/identity/wizard/credentials",
            "api_id=12345&api_hash=deadbeefhash",
            &owner,
        ))
        .await
        .unwrap();
    clear_env_file_var();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/identity/wizard");
    let env = std::fs::read_to_string(&env_path).unwrap();
    assert!(env.contains("TELEGRAM_API_ID=12345"), "{env}");
    assert!(env.contains("TELEGRAM_API_HASH=deadbeefhash"), "{env}");
    assert_eq!(db.get_config("restart_required").as_deref(), Some("1"));
}

#[tokio::test]
async fn unlock_post_without_a_live_actor_redirects_honestly() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/unlock",
            "passphrase=whatever",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    // Not locked → the generic locked-flash target on /identity.
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/identity?error=locked"
    );
    // Success is unreachable here; the referer discipline is covered
    // by the redirect target above.
}

// ---- F2: the shared throttle map is namespaced -------------------

/// A login-page entry can no longer poison the identity-unlock
/// namespace: 5 failed logins AS the literal username an attacker
/// would collide with ("identity-unlock:<owner id>") key
/// `login:identity-unlock:1`, so the owner's unlock POST still
/// reaches the actor (which fails honestly, 303 — the poisoned
/// outcome would be 429).
#[tokio::test]
async fn login_throttle_keys_are_namespaced_against_identity_unlock() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let attempts = state.login_attempts.clone();
    let app = router(state);
    for i in 0..5 {
        let resp = app
            .clone()
            .oneshot(post_req(
                "/login",
                "username=identity-unlock%3A1&password=wrong",
            ))
            .await
            .unwrap();
        // The 5th attempt trips the throttle under its OWN key.
        if i < 4 {
            assert_eq!(resp.status(), StatusCode::OK, "attempt {i}");
        } else {
            assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        }
    }
    let keys = attempts
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        keys.contains("login:identity-unlock:1"),
        "namespaced login key missing: {keys:?}"
    );
    assert!(
        !keys.contains("identity-unlock:1"),
        "bare identity-unlock key exists — unlock is poisonable: {keys:?}"
    );
    // The owner's unlock is therefore not throttled: the unconnected
    // actor answers NotConnected, which redirects (429 would be the
    // poisoned outcome).
    let resp = app
        .oneshot(post_authed_req("/identity/unlock", "passphrase=x", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/identity?error=locked"
    );
}

// ---- F5: wipe-and-repair ------------------------------------------

#[tokio::test]
async fn wipe_route_is_owner_only_audits_and_redirects() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 1).await;
    let app = router(state);
    // Seller: 403 (covered owner-only in the route table above, but
    // this route gets its own pass because it is destructive).
    let resp = app
        .clone()
        .oneshot(post_authed_req("/identity/wipe", "", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // Unauthenticated: 303 /login.
    let resp = app
        .clone()
        .oneshot(post_req("/identity/wipe", ""))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
    // Owner: PRG to /identity even though the unconnected actor
    // refuses the wipe (fails closed) — the page then shows the
    // honest state. The destructive request is audited either way.
    let resp = app
        .oneshot(post_authed_req("/identity/wipe", "", &owner))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/identity");
    let rows = db.list_audit_log(10);
    assert!(
        rows.iter().any(|r| r.3 == "identity_wiped"),
        "identity_wiped audit row missing: {rows:?}"
    );
}

#[tokio::test]
async fn wipe_form_offered_on_ready_and_disabled_with_named_consequence() {
    let (state, _dir) = make_state().await;
    let actions = identity_actions(&state, &ready_status());
    assert!(actions.contains(r#"action="/identity/wipe""#), "{actions}");
    assert!(actions.contains("Wipe stored identity"), "{actions}");
    assert!(
        actions.contains(r#"onclick="return confirm("#),
        "no confirm() guard: {actions}"
    );
    assert!(
            actions.contains(
                "removes the stored personal-identity session on this machine; you will need to pair again"
            ),
            "{actions}"
        );
    // The Disabled panel offers the same recovery — it is the only
    // way back to a connectable state.
    let disabled = crate::identity::IdentityStatus {
        state: crate::identity::IdentityState::Disabled,
        detail: "auth key duplicated elsewhere".to_string(),
        persisted: true,
    };
    let actions = identity_actions(&state, &disabled);
    assert!(actions.contains(r#"action="/identity/wipe""#), "{actions}");
    assert!(
        actions.contains("removes the stored personal-identity session"),
        "{actions}"
    );
    // No identity → the Connect path only, no wipe affordance.
    let not_ready = crate::identity::IdentityStatus {
        state: crate::identity::IdentityState::NotConfigured,
        detail: String::new(),
        persisted: false,
    };
    assert!(!identity_actions(&state, &not_ready).contains("/identity/wipe"));
}

// ---- F7: one-shot flash cookie -------------------------------------

#[tokio::test]
async fn unlock_flash_cookie_helpers_shape_the_one_shot() {
    // The exact response the unlock POST's result arms build. The
    // WrongPassphrase branch itself needs a live actor (the
    // unconnected test handle fails closed with NotConnected), so the
    // set-half is asserted on the helper composition.
    let (state, _dir) = make_state().await;
    let resp = redirect_with_cookie(
        &state.url("/identity"),
        identity_flash_cookie(&state, "unlock_failed"),
    );
    assert!(resp.status().is_redirection());
    let set = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set.starts_with("identity_flash=unlock_failed"), "{set}");
    assert!(set.contains("Max-Age=60"), "{set}");
    assert!(set.contains("HttpOnly"), "{set}");
    assert!(set.contains("SameSite=Strict"), "{set}");
}

/// The flash is consumed exactly once at router level: the response
/// that renders it also clears the cookie (Max-Age=0), a plain GET
/// sets nothing, and the old query-string flags are dead.
#[tokio::test]
async fn identity_flash_cookie_is_consumed_once_and_never_from_query() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    // Plain GET: no identity_flash Set-Cookie, no flash rendered.
    let resp = app
        .clone()
        .oneshot(authed_req("/identity", &owner))
        .await
        .unwrap();
    let set_cookies = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        !set_cookies.contains("identity_flash"),
        "plain GET touched the flash cookie: {set_cookies}"
    );
    let body = body_string(resp).await;
    assert!(!body.contains("Identity unlocked."), "{body}");
    // Query-string spoofing is gone: neither flag renders anything.
    let resp = app
        .clone()
        .oneshot(authed_req("/identity?unlocked=1&error=unlock", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(!body.contains("Identity unlocked."), "{body}");
    assert!(
        !body.contains("That passphrase did not unlock it."),
        "{body}"
    );
    // One cookie header carries both cookies (the session reader
    // takes the first, the flash reader scans the rest).
    let cookie = format!("session={}; identity_flash=unlocked", owner);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/identity")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("identity_flash="))
        .expect("flash cookie not cleared on the response that rendered it")
        .to_string();
    assert!(set.contains("Max-Age=0"), "{set}");
    assert!(set.contains("HttpOnly"), "{set}");
    assert!(set.contains("SameSite=Strict"), "{set}");
    let body = body_string(resp).await;
    assert!(body.contains("Identity unlocked."), "{body}");
    // The unconnected actor's unlock failure path (NotConnected) sets
    // no flash cookie — only the unlock-result arms do.
    let resp = app
        .oneshot(post_authed_req("/identity/unlock", "passphrase=x", &owner))
        .await
        .unwrap();
    let set_cookies = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(
        !set_cookies.contains("identity_flash"),
        "non-result path set a flash cookie: {set_cookies}"
    );
}

// ---- F9: passphrase upper bound ------------------------------------

#[tokio::test]
async fn passphrase_over_128_chars_is_rejected_without_actor_call() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    let long = "x".repeat(129);
    // Unlock banner path: honest alert, NOT the NotConnected
    // redirect — the actor was never consulted.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/unlock",
            &format!("passphrase={long}"),
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = body_string(resp).await;
    assert!(
        body.contains("Passphrase must be at most 128 characters."),
        "{body}"
    );
    assert!(body.contains(r#"role="alert""#), "{body}");
    // Wizard set-passphrase path: same honest refusal, and the typed
    // NotConnected error proves no actor call happened.
    let form = format!("passphrase={long}&confirm_passphrase={long}");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/wizard/passphrase",
            &form,
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("Passphrase must be at most 128 characters."),
        "{body}"
    );
    assert!(
        !body.contains("not running on this install"),
        "actor was consulted for an overlong passphrase: {body}"
    );
    // Boundary: exactly 128 passes the cap and reaches the actor.
    let ok128 = "y".repeat(128);
    let form = format!("passphrase={ok128}&confirm_passphrase={ok128}");
    let resp = app
        .oneshot(post_authed_req(
            "/identity/wizard/passphrase",
            &form,
            &owner,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("not running on this install"), "{body}");
}

// ---- F10: enable-writes requires the ToS flag ----------------------

#[tokio::test]
async fn enable_writes_requires_tos_acceptance_flag() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let app = router(state);
    // Checkbox present, but the wizard's step-1 flag is missing: the
    // alert points there and nothing is written — even though the
    // state gate below it would also have refused (the ToS check
    // runs first, by construction).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/identity/enable-writes",
            "accept=1",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(r#"role="alert""#), "{body}");
    assert!(body.contains("step 1"), "{body}");
    assert!(body.contains(r#"href="/identity/wizard""#), "{body}");
    assert_eq!(db.get_config("identity_writes_enabled"), None);
    // With the flag set the request proceeds past it (and lands on
    // the state gate — the unconnected subsystem reports
    // Disconnected → plain PRG, still no write).
    db.set_config("identity_tos_accepted", "1");
    let resp = app
        .oneshot(post_authed_req(
            "/identity/enable-writes",
            "accept=1",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/identity");
    assert_eq!(db.get_config("identity_writes_enabled"), None);
}

#[tokio::test]
async fn identity_page_renders_honest_states_and_gate() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    // Not configured: one panel, one primary action, no posting gate.
    let resp = app
        .clone()
        .oneshot(authed_req("/identity", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("No personal identity connected."), "{body}");
    assert!(body.contains("Connect personal identity"), "{body}");
    assert!(!body.contains("identity/enable-writes"), "{body}");
    // Status line + rail present; wizard button routes to the door.
    assert!(body.contains("Set up identity"), "{body}");
}

#[tokio::test]
async fn posting_gate_panel_requires_acceptance_then_collapses_to_record() {
    let (state, _dir) = make_state().await;
    let ready = ready_status();
    let panel = identity_posting_panel(&state, &ready, None, None);
    assert!(panel.contains(r#"id="posting-accept""#), "{panel}");
    assert!(
        panel.contains(r#"required aria-required="true""#),
        "{panel}"
    );
    assert!(
        panel.contains(r#"action="/identity/enable-writes""#),
        "{panel}"
    );
    assert!(panel.contains("I understand the risks"), "{panel}");
    // Accepted: collapses to the one-line record, no form left.
    let panel =
        identity_posting_panel(&state, &ready, Some("1"), Some("2026-10-27T00:00:00+00:00"));
    assert!(
        panel.contains(r#"badge badge-success">posting enabled</span>"#),
        "{panel}"
    );
    assert!(panel.contains("Accepted 2026-10-27"), "{panel}");
    assert!(!panel.contains("<form"), "{panel}");
    // No identity → no panel at all.
    let not_ready = crate::identity::IdentityStatus {
        state: crate::identity::IdentityState::NotConfigured,
        detail: String::new(),
        persisted: false,
    };
    assert!(identity_posting_panel(&state, &not_ready, None, None).is_empty());
}

#[test]
fn timeline_nests_replies_one_level_and_escapes() {
    // Newest first, as the actor returns them.
    let msgs = vec![
        tm(14, Some(14), "self", "self reply stays top-level"),
        tm(13, Some(99), "ghost", "parent was clipped off this page"),
        tm(12, Some(10), "bob", "reply to reply"),
        tm(
            11,
            Some(10),
            "<script>alert(1)</script>",
            "first reply with <img src=x>",
        ),
        tm(10, None, "alice", "hello <b>world</b> & friends"),
    ];
    let html = render_timeline(&msgs, "Messages from @chan, newest first");
    assert!(
        html.contains(r#"<ol class="timeline" aria-label="Messages from @chan, newest first">"#),
        "{html}"
    );
    // Exactly one reply sub-list (both replies flattened into it).
    assert_eq!(
        html.matches(r#"<ol aria-label="Replies">"#).count(),
        1,
        "{html}"
    );
    // Escaping: no raw metacharacters from sender or body survive.
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(html.contains("&lt;img src=x&gt;"), "{html}");
    assert!(
        html.contains("hello &lt;b&gt;world&lt;/b&gt; &amp; friends"),
        "{html}"
    );
    assert!(!html.contains("<script>alert"), "{html}");
    assert!(!html.contains("<img src=x>"), "{html}");
    // Thread order inside the sub-list: reply 11 before its reply 12.
    let sub = html.split(r#"<ol aria-label="Replies">"#).nth(1).unwrap();
    let (first_reply_pos, second_reply_pos) = (
        sub.find("first reply").unwrap(),
        sub.find("reply to reply").unwrap(),
    );
    assert!(first_reply_pos < second_reply_pos, "{sub}");
    // Top-level: the self reply and the clipped-parent reply never
    // vanish, and no empty sub-lists render.
    assert!(html.contains("self reply stays top-level"), "{html}");
    assert!(html.contains("parent was clipped off this page"), "{html}");
}

#[tokio::test]
async fn monitor_ready_body_has_exactly_two_get_forms_and_zero_posts() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@testchan", 0).unwrap();
    let msgs = vec![tm(1, None, "alice", "hello")];
    let body = monitor_body(
        &state,
        &ready_status(),
        Some("@testchan"),
        None,
        None,
        &TimelineView::Results(Ok(msgs), false),
    );
    assert_eq!(body.matches("<form").count(), 2, "{body}");
    assert_eq!(body.matches(r#"method="post""#).count(), 0, "{body}");
    assert!(body.contains(r#"method="get""#), "{body}");
    assert!(
        body.contains("read-only · viewing as your personal account"),
        "{body}"
    );
    assert!(body.contains(r#"<div class="mono">"#), "{body}");
    assert!(body.contains(r#"<ol class="timeline""#), "{body}");
    assert!(
        body.contains("That's the oldest message available."),
        "{body}"
    );
    // Selected channel is marked in the picker.
    assert!(
        body.contains(r#"<option value="@testchan" selected>@testchan</option>"#),
        "{body}"
    );
}

#[tokio::test]
async fn monitor_search_and_pagination_shapes() {
    let (state, _dir) = make_state().await;
    state.db.add_channel("@testchan", 0).unwrap();
    // Clipped page: count help + real Older-messages link.
    let msgs: Vec<NormalizedMessage> = (0..100).rev().map(|i| tm(i, None, "a", "m")).collect();
    let body = monitor_body(
        &state,
        &ready_status(),
        Some("@testchan"),
        None,
        None,
        &TimelineView::Results(Ok(msgs), false),
    );
    assert!(body.contains("Showing the latest 100 messages."), "{body}");
    assert!(
        body.contains(r#"href="/monitor?ch=%40testchan&before=0">Older messages</a>"#),
        "{body}"
    );
    // Search: scope line, no pagination (no cursor in the read API).
    let body = monitor_body(
        &state,
        &ready_status(),
        Some("@testchan"),
        Some("needle"),
        None,
        &TimelineView::Results(Ok(vec![tm(5, None, "a", "found needle")]), true),
    );
    assert!(body.contains("<h3>Search results</h3>"), "{body}");
    assert!(
        body.contains("Matches in @testchan · newest first · latest 50."),
        "{body}"
    );
    assert!(!body.contains("Older messages"), "{body}");
    // Zero matches: honest empty state + clear link.
    let body = monitor_body(
        &state,
        &ready_status(),
        None,
        Some("needle"),
        None,
        &TimelineView::Results(Ok(vec![]), true),
    );
    assert!(body.contains("No matches."), "{body}");
    assert!(
        body.contains(r#"href="/monitor">Clear search</a>"#),
        "{body}"
    );
    // Unknown channel: warn, not an error.
    let body = monitor_body(
        &state,
        &ready_status(),
        Some("@gone"),
        None,
        None,
        &TimelineView::Results(
            Err(crate::identity::IdentityError::PeerNotFound("nope".into())),
            false,
        ),
    );
    assert!(
        body.contains("That channel isn't available to your account."),
        "{body}"
    );
}

#[tokio::test]
async fn monitor_not_ready_renders_state_panels_and_no_forms() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    // Subsystem Disconnected: honest not-connected panel; the page
    // contributes zero GET forms (the only <form> on the full page
    // is the rail's bot-scope switcher).
    let resp = app.oneshot(authed_req("/monitor", &owner)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("No personal identity connected."), "{body}");
    assert!(
        body.contains("read-only · viewing as your personal account"),
        "{body}"
    );
    assert_eq!(body.matches(r#"method="get""#).count(), 0, "{body}");
}

// ---- Channel sender binding (IDENTITY_SENDER_UX_SPEC Phase 2) ----

#[tokio::test]
async fn bind_sender_route_is_owner_only() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 1).await;
    let db = state.db.clone();
    db.add_channel("@chan", 0).unwrap();
    let app = router(state);
    // Seller → 403; the binding is the owner's decision (spec §1).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@chan&identity=bot",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // Unauthenticated → 303 /login.
    let resp = app
        .clone()
        .oneshot(post_req(
            "/channels/bind-sender",
            "channel=@chan&identity=bot",
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
    // Owner → PRG back to /channels with the §3 success flash, and
    // the write landed.
    let resp = app
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@chan&identity=bot",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?bound-bot=1&channel=@chan"
    );
    assert_eq!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("bot")
    );
}

#[tokio::test]
async fn bind_sender_user_requires_gate_then_ready() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let db = state.db.clone();
    db.add_channel("@chan", 0).unwrap();
    let app = router(state);
    // Gate unset → rejected with the posting flash; nothing bound
    // (spec §3: the console points at the Enable-posting gate).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@chan&identity=user",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?error=posting"
    );
    assert_ne!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("user")
    );
    // Gate accepted but the subsystem is disconnected (the test
    // handle is unconnected) → honest refusal; still nothing bound.
    db.set_config("identity_writes_enabled", "1");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@chan&identity=user",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?error=identity"
    );
    assert_ne!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("user")
    );
    // Binding back to the bot is always allowed — even disconnected.
    let resp = app
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@chan&identity=bot",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/channels?bound-bot=1&channel=@chan"
    );
    assert_eq!(
        db.get_channel_sender_identity("@chan").as_deref(),
        Some("bot")
    );
    // Both refusals and the bind wrote audit rows (spec §7.2).
    let audit = db.list_audit_log(20);
    assert!(
        audit
            .iter()
            .any(|row| row.3 == "channel_bind_bot" && row.4 == "channel"),
        "{audit:?}"
    );
}

#[tokio::test]
async fn bind_sender_unknown_and_seller_channels_refused() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let db = state.db.clone();
    db.add_channel("@sellerchan", 7).unwrap();
    let app = router(state);
    // Unknown channel → honest inline error, nothing written.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@ghost&identity=bot",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("isn't one of the global channels"), "{body}");
    // A seller-owned row is refused too: the binding must never
    // route seller content through the owner's personal account.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@sellerchan&identity=bot",
            &owner,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("isn't one of the global channels"), "{body}");
    // The column defaults to 'bot' — the point is nothing was bound
    // to the user identity.
    assert_ne!(
        db.get_channel_sender_identity("@sellerchan").as_deref(),
        Some("user")
    );
    // A nonsense identity value gets the same honest inline error.
    let resp = app
        .oneshot(post_authed_req(
            "/channels/bind-sender",
            "channel=@ghost&identity=seller",
            &owner,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("That sender choice isn't one of the console's"),
        "{body}"
    );
}

#[tokio::test]
async fn channels_rows_show_binding_state() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    state.db.add_channel("@botbound", 0).unwrap();
    state.db.add_channel("@userbound", 0).unwrap();
    state.db.set_channel_sender_identity("@userbound", "user");
    let app = router(state);
    let body = body_string(app.oneshot(authed_req("/channels", &owner)).await.unwrap()).await;
    // Exception-only marking: exactly one "as user" badge, on the
    // user-bound row (spec §0.2).
    assert_eq!(body.matches(">as user</span>").count(), 1, "{body}");
    // The user-bound row offers the safe revert with a channel-named
    // aria-label and no confirm.
    assert!(body.contains("Post as bot"), "{body}");
    assert!(
        body.contains(r#"aria-label="Post to @userbound as the bot""#),
        "{body}"
    );
    // The bot-bound row offers the risky direction behind a confirm
    // that names the channel and the risk (§2.1), with a
    // channel-named aria-label (§2.2).
    assert!(body.contains("Post as user…"), "{body}");
    assert!(
        body.contains(r#"aria-label="Post to @botbound as your personal account""#),
        "{body}"
    );
    assert!(
        body.contains(
            "Bind @botbound to your personal account? Posts to this channel will be sent as you"
        ),
        "{body}"
    );
}

#[tokio::test]
async fn dashboard_composer_attributes_user_bound_target() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    state.db.add_channel("@chan", 0).unwrap();
    state.db.set_default_channel("@chan").unwrap();
    let db = state.db.clone();
    let app = router(state);
    // Bot-bound default → plain targeting help, no disclosure.
    let body = body_string(
        app.clone()
            .oneshot(authed_req("/dashboard", &owner))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !body.contains("as your personal account"),
        "unexpected attribution: {body}"
    );
    // User-bound default → the badge plus the pre-send help line
    // (spec §5), computed server-side from the current binding.
    db.set_channel_sender_identity("@chan", "user");
    let body = body_string(app.oneshot(authed_req("/dashboard", &owner)).await.unwrap()).await;
    assert!(
        body.contains(r#"<span class="badge badge-warning">as user</span>"#),
        "{body}"
    );
    assert!(
        body.contains("This will post to @chan as your personal account."),
        "{body}"
    );
}

#[tokio::test]
async fn posts_page_marks_user_sender_rows() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    // One row sent as the user, one as the bot: exactly one badge.
    state
        .db
        .log_post(0, "@userbound", 1, "text", Some("hi"), "user")
        .expect("seed user-sent post");
    state
        .db
        .log_post(0, "@botbound", 2, "text", Some("yo"), "bot")
        .expect("seed bot-sent post");
    let app = router(state);
    let body = body_string(app.oneshot(authed_req("/posts", &owner)).await.unwrap()).await;
    assert_eq!(body.matches(">as user</span>").count(), 1, "{body}");
    assert!(
        body.contains(r#"<span class="badge badge-warning">as user</span></td>"#),
        "{body}"
    );
}

#[test]
fn send_error_flash_keys_are_fixed() {
    use crate::identity::SendError;
    let (key, retry) = send_error_flash(&SendError::FloodWait(42));
    assert_eq!((key, retry), ("flood", Some(42)));
    let (key, retry) = send_error_flash(&SendError::NotAuthorized);
    assert_eq!((key, retry), ("not_authorized", None));
    let (key, retry) = send_error_flash(&SendError::SurfaceHalted);
    assert_eq!((key, retry), ("halted", None));
    let (key, retry) = send_error_flash(&SendError::Transport("x".into()));
    assert_eq!((key, retry), ("transport", None));
    let (key, retry) = send_error_flash(&SendError::WritesDisabled);
    assert_eq!((key, retry), ("writes_disabled", None));
    let (key, retry) = send_error_flash(&SendError::ChatNotFound);
    assert_eq!((key, retry), ("chat", None));
}

#[tokio::test]
async fn channel_chat_id_passes_numeric_through() {
    let (state, _dir) = make_state().await;
    // Numeric Bot-API ids never touch the network, whichever bot
    // would have done the resolving.
    assert_eq!(channel_chat_id(&state.bot, "-100123").await, Some(-100123));
    assert_eq!(channel_chat_id(&state.bot, " 42 ").await, Some(42));
}
