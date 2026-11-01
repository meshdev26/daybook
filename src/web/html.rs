use super::{Session, WebState};
use axum::{
    http::header,
    response::{IntoResponse, Response},
};

const APP_CSS: &str = include_str!("../../assets/app.css");
const FONT_SANS: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-var.woff2");
const FONT_MONO_400: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-400.woff2");
const FONT_MONO_500: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-500.woff2");

const APP_JS: &str = r#"(function () {
  'use strict';
  function init() {
    var toggles = document.querySelectorAll('.secret-toggle');
    for (var i = 0; i < toggles.length; i++) {
      var btn = toggles[i];
      if (btn.dataset.secretBound) continue;
      btn.dataset.secretBound = '1';
      btn.addEventListener('click', function () {
        var field = this.closest('.secret-field');
        var input = field ? field.querySelector('input') : null;
        if (!input) return;
        var show = input.type === 'password';
        // Type swap keeps the typed value; only the rendering changes.
        input.type = show ? 'text' : 'password';
        this.setAttribute('aria-pressed', show ? 'true' : 'false');
        this.setAttribute('aria-label', show ? 'Hide password' : 'Show password');
      });
    }
    // Visibility-aware add form (CHANNEL_ADD_REDESIGN_SPEC §3.3): the radio
    // swaps the identifier placeholder, the visible help line, and the
    // aria-describedby target. Enhancement only — without JS the public help
    // shows by default and the omnibus details still carries both rules.
    var vform = document.querySelector('[data-visibility-form]');
    if (vform && !vform.dataset.visBound) {
      vform.dataset.visBound = '1';
      var id = vform.querySelector('input[name="channel"]');
      var vpub = vform.querySelector('#ch-id-help-public');
      var vpriv = vform.querySelector('#ch-id-help-private');
      vform.querySelectorAll('input[name="visibility"]').forEach(function (r) {
        r.addEventListener('change', function () {
          var priv = this.value === 'private';
          id.placeholder = priv ? id.dataset.phPrivate : id.dataset.phPublic;
          vpriv.hidden = !priv;
          vpub.hidden = priv;
          id.setAttribute('aria-describedby', priv ? 'ch-id-help-private' : 'ch-id-help-public');
        });
      });
    }
    // Focus-on-render for the standing unlock banner and for error alerts
    // that re-render after a failed POST (IDENTITY_UX_SPEC §3.8/§4.2). The
    // containers carry tabindex="-1"; without JS the page still works.
    var banner = document.querySelector('.unlock-banner');
    if (banner && typeof banner.focus === 'function') { banner.focus(); return; }
    var alertEl = document.querySelector('[data-focus-on-load]');
    if (alertEl && typeof alertEl.focus === 'function') { alertEl.focus(); }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
(function () {
  'use strict';
  var KEY = 'tg.sidebar.groups.v1';
  function readState() {
    try { var r = localStorage.getItem(KEY); return r ? (JSON.parse(r) || {}) : {}; }
    catch (e) { return {}; }
  }
  function writeState(s) { try { localStorage.setItem(KEY, JSON.stringify(s)); } catch (e) {} }
  var state = readState();
  var hasStored = Object.keys(state).length > 0;
  function isMobile() { return window.matchMedia('(max-width: 900px)').matches; }
  function groupKey(el) {
    if (!el) return null;
    var v = el.getAttribute('data-nav-group');
    if (v !== null) return v;
    return el.getAttribute('data-group');
  }
  function setGroup(group, open) {
    var el = document.querySelector('.nav-group[data-nav-group="' + group + '"]') || document.querySelector('.nav-group[data-group="' + group + '"]');
    if (!el) return;
    el.classList.toggle('is-collapsed', !open);
    var btn = el.querySelector('.nav-group-toggle');
    if (btn) btn.setAttribute('aria-expanded', open ? 'true' : 'false');
    var panel = el.querySelector('.nav-group-panel');
    if (panel) {
      if (open) panel.removeAttribute('inert'); else panel.setAttribute('inert', '');
      panel.setAttribute('aria-hidden', open ? 'false' : 'true');
    }
  }
  function init() {
    document.querySelectorAll('.nav-group').forEach(function (el) {
      var g = groupKey(el);
      var stored = Object.prototype.hasOwnProperty.call(state, g) ? state[g] : null;
      var open;
      if (stored !== null) open = !!stored;
      else if (isMobile()) open = el.getAttribute('data-current') === 'true';
      else open = true;
      setGroup(g, open);
    });
    document.querySelectorAll('.nav-group-toggle').forEach(function (btn) {
      if (btn.dataset.sidebarBound) return;
      btn.dataset.sidebarBound = '1';
      btn.addEventListener('click', function () {
        var el = this.closest('.nav-group');
        var g = groupKey(el);
        var nowOpen = el.classList.contains('is-collapsed');
        setGroup(g, nowOpen);
        state[g] = nowOpen;
        writeState(state);
      });
    });
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
"#;

const PLANE_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 3.5 3.8 10.2c-.8.3-.8 1.4 0 1.7l4.9 1.8 1.8 4.9c.3.8 1.4.8 1.7 0L19 5.5"/><path d="M10.5 13.7 21 3.5"/></svg>"#;

pub(super) fn js_arg(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

pub(super) fn urlencode_query(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'@' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[tracing::instrument(skip_all)]
pub(super) async fn app_css() -> Response {
    asset_css()
}

#[tracing::instrument(skip_all)]
pub(super) async fn app_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn font_sans() -> Response {
    asset_font(FONT_SANS)
}

#[tracing::instrument(skip_all)]
pub(super) async fn font_mono_400() -> Response {
    asset_font(FONT_MONO_400)
}

#[tracing::instrument(skip_all)]
pub(super) async fn font_mono_500() -> Response {
    asset_font(FONT_MONO_500)
}

pub(super) fn asset_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS).into_response()
}

pub(super) fn asset_font(body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

pub(super) fn html_page(
    state: &WebState,
    session: Option<&Session>,
    title: &str,
    body: &str,
) -> String {
    let base = &state.base_path;
    let snap = super::identity_snapshot();
    let unlock_banner = if session.is_some_and(|s| s.role == "owner") {
        super::identity_unlock_banner(base, &snap).unwrap_or_default()
    } else {
        String::new()
    };
    let identity_flash = if snap.unlocked {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Identity unlocked.</div>"#
    } else {
        ""
    };
    let identity_line = super::identity_status_line(base, session, &snap.status);
    let bot_scope = format!(
        "{}{}",
        super::bot_scope_control(state, session),
        identity_line
    );
    format!(
        r##"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Telegram Business</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<link rel="preload" href="{base}/assets/fonts/IBMPlexSans-var.woff2" as="font" type="font/woff2" crossorigin>
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<a class="skip-link" href='#main'>Skip to content</a>
<div class="shell">
<aside class="rail">
  <div class="brand">{plane}<b>Telegram Business<small>operator console</small></b></div>
  {bot_scope}
  <nav>{nav}</nav>
  <div class="foot"><a href="{base}/settings">Settings</a> <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()">Theme</button></div>
</aside>
<main class="main" id="main"><div class="measure">{unlock_banner}{identity_flash}
{b}
</div></main>
</div>
<script>function toggleTheme(){{var c=document.documentElement.getAttribute('data-theme');var n=c==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);localStorage.setItem('theme',n);}}</script>
</body></html>"##,
        base = base,
        t = title,
        b = body,
        plane = PLANE_SVG,
        bot_scope = bot_scope,
        nav = rail_nav(state, title, session),
        unlock_banner = unlock_banner,
        identity_flash = identity_flash,
    )
}

pub(super) fn html_page_bare(base: &str, title: &str, body: &str) -> String {
    html_page_bare_panel(base, title, body, "panel")
}

pub(super) fn html_page_bare_panel(
    base: &str,
    title: &str,
    body: &str,
    panel_class: &str,
) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Telegram Business</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<a class="skip-link" href='#main'>Skip to content</a>
<main class="door" id="main">
  <div class="brand">{plane}<b>Telegram Business</b></div>
  <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()">Theme</button>
  <div class="{panel_class}">{b}</div>
</main>
<script>function toggleTheme(){{var c=document.documentElement.getAttribute('data-theme');var n=c==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);localStorage.setItem('theme',n);}}</script>
</body></html>"#,
        base = base,
        t = title,
        b = body,
        plane = PLANE_SVG,
        panel_class = panel_class,
    )
}

pub(super) fn nav_link(out: &mut String, base: &str, path: &str, label: &str, title: &str) {
    let active = if title.eq_ignore_ascii_case(label) {
        " class=\"active\" aria-current=\"page\""
    } else {
        ""
    };
    out.push_str(&format!("<a href=\"{base}/{path}\"{active}>{label}</a>"));
}

pub(super) fn nav_links(out: &mut String, base: &str, title: &str, items: &[(&str, &str)]) {
    for (path, label) in items {
        nav_link(out, base, path, label, title);
    }
}

pub(super) fn rail_nav(state: &WebState, title: &str, session: Option<&Session>) -> String {
    let base = &state.base_path;
    let mut manage: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = session.filter(|s| s.role == "owner") {
        let p = super::effective_sidebar_prefs(state, s);
        if p.ai {
            manage.push(("ai", "AI"));
        }
        if p.crypto {
            manage.push(("crypto", "Crypto"));
        }
        if p.admins {
            manage.push(("admins", "Admins"));
        }
        if p.sellers {
            manage.push(("sellers", "Sellers"));
        }
        if p.webusers {
            manage.push(("webusers", "Web users"));
        }
    }
    let mut out = String::new();
    // Operate
    {
        let current = ["Dashboard", "Channels", "Plans", "Products", "Bots"]
            .iter()
            .any(|l| title.eq_ignore_ascii_case(l));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"operate\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-operate"><span class="nav-group-label">Operate</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-operate">"#);
        nav_links(
            &mut out,
            base,
            title,
            &[
                ("dashboard", "Dashboard"),
                ("channels", "Channels"),
                ("plans", "Plans"),
                ("products", "Products"),
            ],
        );
        if session.is_some_and(|s| s.role == "owner") {
            nav_link(&mut out, base, "bots", "Bots", title);
        }
        out.push_str("</div></section>");
    }
    // Identity (owner only)
    if session.is_some_and(|s| s.role == "owner") {
        let current = ["Identity", "Monitor"]
            .iter()
            .any(|l| title.eq_ignore_ascii_case(l));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"identity\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-identity"><span class="nav-group-label">Identity</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-identity">"#);
        nav_links(
            &mut out,
            base,
            title,
            &[("identity", "Identity"), ("monitor", "Monitor")],
        );
        out.push_str("</div></section>");
    }
    // Sell
    {
        let current = [
            "Codes",
            "Promos",
            "Referrals",
            "Orders",
            "Finance",
            "Customers",
            "Cart",
        ]
        .iter()
        .any(|l| title.eq_ignore_ascii_case(l));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"sell\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-sell"><span class="nav-group-label">Sell</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-sell">"#);
        nav_links(
            &mut out,
            base,
            title,
            &[
                ("codes", "Codes"),
                ("promos", "Promos"),
                ("referrals", "Referrals"),
                ("orders", "Orders"),
                ("finance", "Finance"),
                ("customers", "Customers"),
                ("cart", "Cart"),
            ],
        );
        out.push_str("</div></section>");
    }
    // Fulfill
    {
        let current = ["Subscriptions", "Bookings", "Tickets"]
            .iter()
            .any(|l| title.eq_ignore_ascii_case(l));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"fulfill\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-fulfill"><span class="nav-group-label">Fulfill</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-fulfill">"#);
        nav_links(
            &mut out,
            base,
            title,
            &[
                ("subs", "Subscriptions"),
                ("bookings", "Bookings"),
                ("tickets", "Tickets"),
            ],
        );
        out.push_str("</div></section>");
    }
    // Insights
    {
        let current = ["Analytics", "Posts", "Broadcast"]
            .iter()
            .any(|l| title.eq_ignore_ascii_case(l));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"insights\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-insights"><span class="nav-group-label">Insights</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-insights">"#);
        nav_links(
            &mut out,
            base,
            title,
            &[
                ("analytics", "Analytics"),
                ("posts", "Posts"),
                ("broadcast", "Broadcast"),
            ],
        );
        out.push_str("</div></section>");
    }
    if session.is_some_and(|s| s.role == "seller" || super::is_admin_role(&s.role)) {
        nav_link(&mut out, base, "keys", "API keys", title);
    }
    if session.is_some_and(|s| s.role == "seller" || super::is_admin_role(&s.role)) {
        nav_link(&mut out, base, "carts", "Carts", title);
    }
    if !manage.is_empty() {
        let current = manage
            .iter()
            .any(|(_, label)| title.eq_ignore_ascii_case(label));
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"manage\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-manage"><span class="nav-group-label">Manage</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
        );
        out.push_str(r#"<div class="nav-group-panel" id="nav-panel-manage">"#);
        for (path, label) in &manage {
            nav_link(&mut out, base, path, label, title);
        }
        out.push_str("</div></section>");
    }
    out
}
