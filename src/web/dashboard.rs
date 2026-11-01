use super::*;
/// The persistent restart reminder. Setup and every settings save that
/// writes .env set `restart_required`; the running process keeps the config
/// snapshot it started with, so a saved value is not live until the operator
/// restarts. Nothing clears the flag (the console cannot observe a restart)
/// — the banner stays until then, the honest cost of a config file read
/// once at boot. Read by /dashboard and /settings.
pub(super) fn restart_required_banner(state: &WebState) -> String {
    if state.db.get_config("restart_required").as_deref() == Some("1") {
        r#"<div class="alert alert-warn" role="status" aria-live="polite">Settings saved — restart the app to apply. Desktop: quit and reopen. Server (systemd): <code>systemctl restart telegram-agent</code>. Docker: <code>docker compose restart</code>.</div>"#
            .to_string()
    } else {
        String::new()
    }
}

/// Row ceiling for the bot-scoped dashboard counts. The checklist only asks
/// "has any", but a count must not silently under-report when a console holds
/// more subscriptions than one list fetch should carry — the
/// `BOOKINGS_COUNT_LIMIT` precedent.
pub(super) const DASHBOARD_COUNT_LIMIT: i64 = 100_000;

#[derive(Deserialize)]
pub(super) struct DashboardQuery {
    /// Setup-claim success flash (SETUP_MCP_UX_SPEC §3.2/§10.5): the claim
    /// POST lands here with ?welcome=1. Carries no secret and no state.
    pub(super) welcome: Option<String>,
    /// create_post failure flash (IDENTITY_SENDER_UX_SPEC §3 honesty on the
    /// composer's own surface): fixed keys rendered server-side, retry
    /// seconds ride separately — no free text, no PII in the URL (§7.5).
    pub(super) post_error: Option<String>,
    pub(super) post_retry: Option<u64>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn dashboard(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let scope = bot_scope_sellers(&state, &session);
    let scoped_bot = if scope.is_some() {
        session.selected_bot_id
    } else {
        None
    };
    let ch = dashboard_default_channel(&state, scoped_bot);
    let titles = channel_titles(&state);
    let lists = dashboard_lists(&state, seller_scope, &scope);
    let banners = dashboard_banners(&state, &session, &q);
    let (channel_options, composer_note) =
        dashboard_channel_options(&state, &session, seller_scope).await;
    let onboarding = dashboard_onboarding(
        &state,
        lists.has_product,
        lists.total_subs_count,
        lists.active_subs_count,
    );
    let connect_banner = banners.connect;
    let restart_banner = banners.restart;
    let welcome_banner = banners.welcome;
    let post_error_banner = banners.post_error;
    let body = format!(
        r#"<h1>Dashboard <span class="sub">({})</span></h1>
{welcome_banner}
{post_error_banner}
{connect_banner}
{restart_banner}
{onboarding}
<article class="panel"><div class="stat"><span class="stat-label">Default channel</span><span class="stat-value">{}</span></div></article>
<div class="grid-2">
<article class="panel"><h3>Plans ({})</h3><ul>{}</ul><a class="btn btn-ghost btn-sm" href="{}">Create on the Plans page</a></article>
<article class="panel"><h3>Recent subscriptions ({})</h3><ul>{}</ul></article>
<article class="panel"><h3>Post</h3><form method="post" action="{}">
<label>Channel<select name="channel">{}</select>{composer_note}</label>
<label>Text<textarea name="text" required></textarea></label>
<button type="submit">Post</button></form></article>
</div>"#,
        esc(&session.role),
        channel_label(
            titles.get(&ch.to_ascii_lowercase()).map(String::as_str),
            &ch
        ),
        lists.plans_len,
        lists.plans_list,
        state.url("/plans"),
        lists.subs_len,
        lists.subs_list,
        state.url("/post"),
        channel_options,
    );
    Html(html_page(&state, Some(&session), "Dashboard", &body)).into_response()
}

struct DashboardLists {
    plans_len: usize,
    subs_len: usize,
    plans_list: String,
    subs_list: String,
    has_product: bool,
    total_subs_count: i64,
    active_subs_count: i64,
}

fn dashboard_default_channel(state: &WebState, scoped_bot: Option<i64>) -> String {
    match scoped_bot {
        Some(bid) => state.db.default_channel_for_bot(bid),
        None => state.db.default_channel(),
    }
    .unwrap_or_else(|| "(none)".into())
}

fn dashboard_lists(
    state: &WebState,
    seller_scope: Option<i64>,
    scope: &Option<Vec<i64>>,
) -> DashboardLists {
    let mut plans = state.db.list_plans(seller_scope);
    plans.retain(|p| in_bot_scope(scope, p.6));
    let subs = match scope {
        Some(set) => state.db.list_subscriptions_for_sellers(set, 10),
        None => state.db.list_subscriptions(seller_scope, 10),
    };
    let plans_list = if plans.is_empty() {
        // C10b: the dashboard no longer duplicates the Plans create form —
        // the ghost link above is the one pointer, so the empty state points
        // there too.
        "<li class=\"empty\"><b>No plans yet.</b>Create your first plan on the Plans page — buyers see it in the shop.</li>"
            .to_string()
    } else {
        plans
            .iter()
            .map(|p| {
                format!(
                    "<li><b>{}</b><span class=\"mono\">{} {}</span><span class=\"help\">/ {} days</span></li>",
                    esc(&p.0),
                    esc(&p.1),
                    esc(&p.2),
                    p.3
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let subs_list = if subs.is_empty() {
        "<li class=\"empty\"><b>No subscriptions yet.</b>They appear here the moment a buyer activates.</li>"
            .to_string()
    } else {
        subs.iter()
            .map(|s| {
                format!(
                    "<li><span class=\"mono\">#{}</span> user <span class=\"mono\">{}</span> <span class=\"mono\">{}</span> <span class=\"help\">expires {}</span></li>",
                    s.0,
                    s.1,
                    esc(&s.3),
                    esc(&s.4)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let mut products = state.db.list_products(seller_scope);
    products.retain(|p| in_bot_scope(scope, p.1));
    let has_product = !plans.is_empty() || !products.is_empty();
    let (total_subs_count, active_subs_count) = match scope {
        Some(set) => {
            let all = state
                .db
                .list_subscriptions_for_sellers(set, DASHBOARD_COUNT_LIMIT);
            let active = all.iter().filter(|s| s.4 == "active").count() as i64;
            (all.len() as i64, active)
        }
        None => (
            state.db.count_subscriptions(seller_scope, None),
            state.db.count_subscriptions(seller_scope, Some("active")),
        ),
    };
    DashboardLists {
        plans_len: plans.len(),
        subs_len: subs.len(),
        plans_list,
        subs_list,
        has_product,
        total_subs_count,
        active_subs_count,
    }
}

struct DashboardBanners {
    connect: String,
    restart: String,
    welcome: String,
    post_error: String,
}

fn dashboard_banners(state: &WebState, session: &Session, q: &DashboardQuery) -> DashboardBanners {
    let connect_banner = if state.cfg.is_configured() {
        String::new()
    } else {
        format!(
            r#"<div class="alert alert-info" role="status" aria-live="polite">The bot is not connected yet. Add your bot token in <a href="{}">Settings</a> — it connects on the next restart.</div>"#,
            state.url("/settings")
        )
    };
    let restart_banner = restart_required_banner(state);
    let welcome_banner = if q.welcome.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Password set — welcome aboard.</div>"#
    } else {
        ""
    };
    let post_error_banner = match q.post_error.as_deref() {
        Some(key) => {
            let secs = q.post_retry.unwrap_or(0);
            let msg = match key {
                "writes_disabled" => {
                    if session.role == "owner" {
                        format!(
                            "Posting as your personal account isn't enabled yet. Enable it on the <a href=\"{}\">Identity page</a> first.",
                            state.url("/identity")
                        )
                    } else {
                        "Posting as your personal account isn't enabled yet — the console owner enables it on the Identity page first.".to_string()
                    }
                }
                "cap" => format!("Posting cap reached — try again in {secs} seconds."),
                "flood" => format!("Telegram flood control — try again in {secs} seconds."),
                "not_authorized" => {
                    "The personal identity is not signed in — the post was not sent.".to_string()
                }
                "halted" => "Personal identity sends are halted after repeated Telegram flood control — an owner surface reset (or restart) is required.".to_string(),
                "chat" => {
                    "That channel isn't available to the sender — check the channel name.".to_string()
                }
                "resolve" => {
                    "The channel's Telegram id could not be resolved — check the channel name.".to_string()
                }
                "transport" => {
                    "A Telegram connection problem occurred — the post was not sent; try again shortly.".to_string()
                }
                _ => "The post was not sent — try again shortly.".to_string(),
            };
            format!(
                r#"<div class="alert alert-error" role="alert" tabindex="-1" data-focus-on-load>{msg}</div>"#
            )
        }
        None => String::new(),
    };
    DashboardBanners {
        connect: connect_banner,
        restart: restart_banner,
        welcome: welcome_banner.to_string(),
        post_error: post_error_banner,
    }
}

async fn dashboard_channel_options(
    state: &WebState,
    session: &Session,
    seller_scope: Option<i64>,
) -> (String, String) {
    let mut channel_options = String::from("<option value=\"\">(default)</option>");
    let mut seen: Vec<String> = Vec::new();
    let binding_suffix = |channel: &str| -> &'static str {
        if state.db.get_channel_sender_identity(channel).as_deref() == Some("user") {
            " · as user"
        } else {
            ""
        }
    };
    let titles = channel_titles(state);
    for row in state.db.list_channels(seller_scope) {
        seen.push(row.channel.clone());
        let suffix = binding_suffix(&row.channel);
        channel_options.push_str(&format!(
            "<option value=\"{esc_ch}\">{label}{suffix}</option>",
            esc_ch = esc(&row.channel),
            label = channel_label(row.title.as_deref(), &row.channel),
            suffix = suffix
        ));
    }
    if let Some(sid) = seller_scope
        && let Some(own) = state.db.seller_channel(sid)
        && !seen.iter().any(|c| c.eq_ignore_ascii_case(&own))
    {
        let suffix = binding_suffix(&own);
        channel_options.push_str(&format!(
            "<option value=\"{0}\">{1}{2}</option>",
            esc(&own),
            channel_label(
                titles.get(&own.to_ascii_lowercase()).map(String::as_str),
                &own
            ),
            suffix
        ));
    }
    let composer_note = match state.db.default_channel() {
        Some(ch) if state.db.get_channel_sender_identity(&ch).as_deref() == Some("user") => {
            if session.role == "owner" {
                let identity_locked =
                    state.identity.status().await.state == IdentityState::Locked;
                let label = channel_label(
                    titles.get(&ch.to_ascii_lowercase()).map(String::as_str),
                    &ch,
                );
                let line = if identity_locked {
                    format!(
                        "This will post to {} as your personal account — which is locked. Unlock it in the banner above.",
                        label
                    )
                } else {
                    format!("This will post to {} as your personal account.", label)
                };
                format!(
                    "<span class=\"help\"><span class=\"badge badge-warning\">as user</span> {line}</span>"
                )
            } else {
                "<span class=\"help\">Posting to this channel happens under the console owner's personal account and is owner-only.</span>"
                    .to_string()
            }
        }
        _ => "<span class=\"help\">Leave on (default) to post where the default channel points.</span>"
            .to_string(),
    };
    (channel_options, composer_note)
}

fn dashboard_onboarding(
    state: &WebState,
    has_product: bool,
    total_subs_count: i64,
    active_subs_count: i64,
) -> String {
    let has_test_buy = total_subs_count > 0;
    let has_delivery = active_subs_count > 0;
    let chk = |done: bool| {
        if done {
            r#"<svg class="icon-check" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="20 6 9 17 4 12"/></svg><span class="sr-only">Done</span>"#
        } else {
            r#"<svg class="icon-empty" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/></svg><span class="sr-only">To-do</span>"#
        }
    };
    format!(
        r#"<article class="panel"><h3>Onboarding checklist</h3>
<ul>
<li>{} <a href="{}">Create product</a></li>
<li>{} Test-buy <span class="mono">/buy &lt;name&gt;</span></li>
<li>{} <a href="{}">Verify delivery</a></li>
</ul></article>"#,
        chk(has_product),
        state.url("/products"),
        chk(has_test_buy),
        chk(has_delivery),
        state.url("/orders"),
    )
}
