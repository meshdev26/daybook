use super::{Session, WebState, capability_enabled, effective_bot};
use crate::roles::Role;
use axum::{
    http::header,
    response::{IntoResponse, Response},
};

const APP_CSS: &str = include_str!("../../assets/app.css");
const FONT_SANS: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-var.woff2");
const FONT_MONO_400: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-400.woff2");
const FONT_MONO_500: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-500.woff2");

pub(super) const APP_JS: &str = r#"(function () {
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
    // Radio-only enhancement: swap the identifier help and aria-describedby.
    // Without JS the public help and omnibus rules still render.
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
    // Focus the unlock banner or a re-rendered error alert; the containers
    // carry tabindex="-1", and the page still works without JS.
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
    // Mobile flattens groups and hides the toggle; force them open so visible
    // links stay navigable. Desktop keeps its stored ARIA state.
    if (isMobile()) open = true;
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
  function applyStored() {
    document.querySelectorAll('.nav-group').forEach(function (el) {
      var g = groupKey(el);
      var stored = Object.prototype.hasOwnProperty.call(state, g) ? state[g] : null;
      setGroup(g, stored !== null ? !!stored : true);
    });
  }
  function init() {
    applyStored();
    document.querySelectorAll('.nav-group-toggle').forEach(function (btn) {
      if (btn.dataset.sidebarBound) return;
      btn.dataset.sidebarBound = '1';
      btn.addEventListener('click', function () {
        // The toggle is display:none on mobile; never record a phantom collapse
        // that would resurface as a closed group after returning to desktop.
        if (isMobile()) return;
        var el = this.closest('.nav-group');
        var g = groupKey(el);
        var nowOpen = el.classList.contains('is-collapsed');
        setGroup(g, nowOpen);
        state[g] = nowOpen;
        writeState(state);
      });
    });
    // Crossing the breakpoint re-evaluates every group: desktop collapses
    // become fully navigable mobile links, and the stored preference returns
    // on the way back.
    var mq = window.matchMedia('(max-width: 900px)');
    if (mq.addEventListener) mq.addEventListener('change', applyStored);
    else if (mq.addListener) mq.addListener(applyStored);
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
(function () {
  'use strict';
  // Client-only product filter: no `name`, no submit, no persistence.
  // Without JS every product stays grouped by type in the Product select.
  function init() {
    var typeSel = document.getElementById('scope-type');
    var prodSel = document.getElementById('scope-product');
    if (!typeSel || !prodSel) return;
    function apply() {
      var t = typeSel.value;
      Array.prototype.forEach.call(prodSel.options, function (o) {
        // The "All products" reset must always remain reachable.
        if (!o.value || !o.getAttribute('data-type')) return;
        o.hidden = !!t && o.getAttribute('data-type') !== t;
      });
      Array.prototype.forEach.call(prodSel.querySelectorAll('optgroup'), function (g) {
        var any = false;
        Array.prototype.forEach.call(g.options, function (o) { if (!o.hidden) any = true; });
        g.disabled = !!t && !any;
      });
      var chosen = prodSel.options[prodSel.selectedIndex];
      if (t && chosen && chosen.hidden) prodSel.value = '';
    }
    typeSel.addEventListener('change', apply);
    apply();
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();
})();
(function () {
  'use strict';
  // Theme is a two-state toggle: the pressed state is the fact for screen
  // readers. The inline head script resolves light/dark before paint; this
  // deferred pass mirrors that resolved value onto every Theme control.
  function syncThemeButtons() {
    var dark = document.documentElement.getAttribute('data-theme') === 'dark';
    document.querySelectorAll('.theme-toggle').forEach(function (b) {
      b.setAttribute('aria-pressed', dark ? 'true' : 'false');
      b.setAttribute('aria-label', dark ? 'Switch to light theme' : 'Switch to dark theme');
    });
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', syncThemeButtons);
  else syncThemeButtons();
})();
(function () {
  'use strict';
  // Remember scroll offset per URL across full navigations; back/forward and
  // never-visited pages stay native (scrollRestoration untouched).
  var KEY = 'tg.scroll.v1';
  var navType = (function () {
    try {
      var e = performance.getEntriesByType('navigation')[0];
      return e && e.type ? e.type : 'navigate';
    } catch (_) { return 'navigate'; }
  })();
  function urlKey() { return location.pathname + location.search; }
  function read() {
    try {
      var m = JSON.parse(sessionStorage.getItem(KEY) || '{}');
      return m && typeof m === 'object' ? m : {};
    } catch (_) { return {}; }
  }
  function save() {
    try {
      var m = read();
      m[urlKey()] = window.scrollY || window.pageYOffset || 0;
      sessionStorage.setItem(KEY, JSON.stringify(m));
    } catch (_) { /* storage blocked — enhancement only */ }
  }
  if (navType !== 'back_forward') {
    var y = read()[urlKey()];
    if (typeof y === 'number' && y > 0) {
      requestAnimationFrame(function () { window.scrollTo({ top: y, behavior: 'instant' }); });
    }
  }
  window.addEventListener('pagehide', save);
  window.addEventListener('beforeunload', save);
})();
(function () {
  'use strict';
  // Remember the desktop rail's scrollTop across navigations so a long nav
  // resumes; mobile and short rails stay untouched. No-JS: top.
  var KEY = 'tg.rail.scroll.v1';
  function isDesktop() { return window.matchMedia('(min-width: 901px)').matches; }
  function rail() { return document.querySelector('.rail'); }
  function save() {
    if (!isDesktop()) return;
    var el = rail();
    if (!el || el.scrollHeight <= el.clientHeight) return;
    try { sessionStorage.setItem(KEY, String(el.scrollTop)); }
    catch (_) { /* storage blocked — enhancement only */ }
  }
  if (isDesktop()) {
    var y = 0;
    try { y = parseInt(sessionStorage.getItem(KEY), 10) || 0; }
    catch (_) { y = 0; }
    if (y > 0) {
      requestAnimationFrame(function () {
        var el = rail();
        if (!el) return;
        if (typeof el.scrollTo === 'function') {
          try { el.scrollTo({ top: y, behavior: 'instant' }); }
          catch (_) { el.scrollTop = y; }
        } else { el.scrollTop = y; }
      });
    }
  }
  window.addEventListener('pagehide', save);
  window.addEventListener('beforeunload', save);
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
    let unlock_banner =
        if session.is_some_and(|s| Role::from_role_str(&s.role).has_level(Role::Owner)) {
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
        r##"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Daybook</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<link rel="preload" href="{base}/assets/fonts/IBMPlexSans-var.woff2" as="font" type="font/woff2" crossorigin>
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<a class="skip-link" href='#main'>Skip to content</a>
<div class="shell">
<aside class="rail">
  <div class="brand">{plane}<b>Daybook<small>operator console</small></b></div>
  {bot_scope}
  <nav>{nav}</nav>
</aside>
<main class="main" id="main"><div class="measure">{unlock_banner}{identity_flash}
{b}
</div></main>
</div>
<script>function toggleTheme(){{var c=document.documentElement.getAttribute('data-theme');var n=c==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);localStorage.setItem('theme',n);document.querySelectorAll('.theme-toggle').forEach(function(b){{b.setAttribute('aria-pressed',n==='dark'?'true':'false');b.setAttribute('aria-label',n==='dark'?'Switch to light theme':'Switch to dark theme');}});}}</script>
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
        r#"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Daybook</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<a class="skip-link" href='#main'>Skip to content</a>
<main class="door" id="main">
  <div class="brand">{plane}<b>Daybook</b></div>
  <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()" aria-pressed="false" aria-label="Switch to dark theme">Theme</button>
  <div class="{panel_class}">{b}</div>
</main>
<script>function toggleTheme(){{var c=document.documentElement.getAttribute('data-theme');var n=c==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);localStorage.setItem('theme',n);document.querySelectorAll('.theme-toggle').forEach(function(b){{b.setAttribute('aria-pressed',n==='dark'?'true':'false');b.setAttribute('aria-label',n==='dark'?'Switch to light theme':'Switch to dark theme');}});}}</script>
</body></html>"#,
        base = base,
        t = title,
        b = body,
        plane = PLANE_SVG,
        panel_class = panel_class,
    )
}

#[derive(Clone, Copy)]
pub(super) enum NavRole {
    All,
    Owner,
    SellerAdminOwner,
}

impl NavRole {
    pub(super) fn visible(self, role: Role) -> bool {
        match self {
            NavRole::All => true,
            NavRole::Owner => role.has_level(Role::Owner),
            // Seller/Admin/Owner share the non-buyer tier. Ordered level
            // check: a role that outranks Seller is included without naming
            // each role string.
            NavRole::SellerAdminOwner => role.has_level(Role::Seller),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct NavItem {
    pub(super) id: &'static str,
    pub(super) label: &'static str,
    pub(super) path: &'static str,
    pub(super) group: &'static str,
    pub(super) roles: NavRole,
    pub(super) titles: &'static [&'static str],
    pub(super) pref: Option<&'static str>,
    /// Compile-time capability this item belongs to. `None` = always
    /// compiled (the shipped default build); `rail_nav` drops items whose
    /// feature was not compiled in. View filter only — never a page gate.
    pub(super) feature: Option<&'static str>,
    /// Product-type metadata values this item applies to. `None` = all
    /// types. A selected product narrows the view; "All products" (no
    /// selection) never narrows it.
    pub(super) product_types: Option<&'static [&'static str]>,
}

pub(super) const NAV_ITEMS: &[NavItem] = &[
    // Overview
    NavItem {
        id: "dashboard",
        label: "Dashboard",
        path: "dashboard",
        group: "Overview",
        roles: NavRole::All,
        titles: &["Dashboard"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "analytics",
        label: "Analytics",
        path: "analytics",
        group: "Overview",
        roles: NavRole::All,
        titles: &["Analytics"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "monitor",
        label: "Monitor",
        path: "monitor",
        group: "Overview",
        roles: NavRole::Owner,
        titles: &["Monitor"],
        pref: None,
        feature: None,
        product_types: None,
    },
    // Offerings
    NavItem {
        id: "products",
        label: "Products",
        path: "products",
        group: "Offerings",
        roles: NavRole::All,
        titles: &["Products"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "plans",
        label: "Plans",
        path: "plans",
        group: "Offerings",
        roles: NavRole::All,
        titles: &["Plans"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "channels",
        label: "Channels",
        path: "channels",
        group: "Offerings",
        roles: NavRole::All,
        titles: &["Channels"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "bots",
        label: "Bots",
        path: "bots",
        group: "Offerings",
        roles: NavRole::Owner,
        titles: &["Bots", "Add bot"],
        pref: None,
        feature: None,
        product_types: None,
    },
    // Sales
    NavItem {
        id: "orders",
        label: "Orders",
        path: "orders",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Orders"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "customers",
        label: "Customers",
        path: "customers",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Customers"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "codes",
        label: "Codes",
        path: "codes",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Codes"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "promos",
        label: "Promos",
        path: "promos",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Promos"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "referrals",
        label: "Referrals",
        path: "referrals",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Referrals", "Reviews"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "cart",
        label: "My cart",
        path: "cart",
        group: "Sales",
        roles: NavRole::All,
        titles: &["Cart"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "carts",
        label: "Abandoned carts",
        path: "carts",
        group: "Sales",
        roles: NavRole::SellerAdminOwner,
        titles: &["Carts"],
        pref: None,
        feature: None,
        product_types: None,
    },
    // Fulfill
    NavItem {
        id: "subs",
        label: "Subscriptions",
        path: "subs",
        group: "Fulfill",
        roles: NavRole::All,
        titles: &["Subscriptions"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "bookings",
        label: "Bookings",
        path: "bookings",
        group: "Fulfill",
        roles: NavRole::All,
        titles: &["Bookings"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "tickets",
        label: "Tickets",
        path: "tickets",
        group: "Fulfill",
        roles: NavRole::All,
        titles: &["Tickets"],
        pref: None,
        feature: None,
        product_types: None,
    },
    // Communicate
    NavItem {
        id: "posts",
        label: "Posts",
        path: "posts",
        group: "Communicate",
        roles: NavRole::All,
        titles: &["Posts"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "broadcast",
        label: "Broadcast",
        path: "broadcast",
        group: "Communicate",
        roles: NavRole::All,
        titles: &["Broadcast"],
        pref: None,
        feature: None,
        product_types: None,
    },
    // Workspace
    NavItem {
        id: "identity",
        label: "Identity",
        path: "identity",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Identity"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "ai",
        label: "AI",
        path: "ai",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["AI"],
        pref: Some("ai"),
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "crypto",
        label: "Crypto",
        path: "crypto",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Crypto"],
        pref: Some("crypto"),
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "finance",
        label: "Finance",
        path: "finance",
        group: "Workspace",
        roles: NavRole::SellerAdminOwner,
        titles: &["Finance"],
        pref: None,
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "admins",
        label: "Admins",
        path: "admins",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Admins"],
        pref: Some("admins"),
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "sellers",
        label: "Sellers",
        path: "sellers",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Sellers"],
        pref: Some("sellers"),
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "webusers",
        label: "Web users",
        path: "webusers",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Web users"],
        pref: Some("webusers"),
        feature: None,
        product_types: None,
    },
    NavItem {
        id: "apikeys",
        label: "API keys",
        path: "keys",
        group: "Workspace",
        roles: NavRole::SellerAdminOwner,
        titles: &["API keys"],
        pref: None,
        feature: None,
        product_types: None,
    },
];

/// Capability items are injected into `rail_nav` after their anchor row only
/// when the capability is compiled in; page handlers carry their own gate.
pub(super) const FEATURE_NAV_ITEMS: &[NavItem] = &[
    NavItem {
        id: "blocked-words",
        label: "Blocked words",
        path: "blocked-words",
        group: "Offerings",
        roles: NavRole::SellerAdminOwner,
        titles: &["Blocked words"],
        pref: None,
        feature: Some("moderation"),
        product_types: None,
    },
    NavItem {
        id: "content-policy",
        label: "Content policy",
        path: "content-policy",
        group: "Workspace",
        roles: NavRole::Owner,
        titles: &["Content policy"],
        pref: None,
        feature: Some("moderation"),
        product_types: None,
    },
];

/// Stable insertion anchors for the capability rows: "Blocked words" follows
/// Bots in Offerings, "Content policy" sits beside AI in Workspace.
pub(super) const FEATURE_NAV_AFTER: &[(&str, &str)] =
    &[("blocked-words", "bots"), ("content-policy", "ai")];

pub(super) fn active_nav_id(title: &str) -> Option<&'static str> {
    // Settings is owner-only and lives in Utilities (outside NAV_ITEMS), but
    // its active state is still resolved here by id so no title/label
    // comparison leaks into the render code.
    if ["Settings"].contains(&title) {
        return Some("settings");
    }
    NAV_ITEMS
        .iter()
        .chain(FEATURE_NAV_ITEMS.iter())
        .find(|item| item.titles.contains(&title))
        .map(|item| item.id)
}

/// The product type metadata of the session's resolved scope, or `None` for
/// the unscoped "All products" view. Sellers/admins are pinned to their bot;
/// an owner with no selection is unscoped and therefore never type-narrowed.
/// This is view metadata only — it changes no page gate or write permission.
fn selected_product_type(state: &WebState, session: Option<&Session>) -> Option<String> {
    let s = session?;
    let role = Role::from_role_str(&s.role);
    // Exact-level branches: the original gave owners their explicit
    // selection, sellers/admins their pinned bot, and buyers nothing. An
    // ordered `has_level(Seller)` here would wrongly catch the owner branch,
    // so each branch keeps its exact level.
    let bot_id = if role == Role::Owner {
        s.selected_bot_id
    } else if role == Role::Seller || role == Role::Admin {
        Some(effective_bot(state, s).id)
    } else {
        None
    };
    bot_id
        .and_then(|id| state.db.get_bot(id))
        .map(|b| b.bot_type)
}

/// Runtime type filter. `None` on either side admits the item: an unscoped
/// owner sees every item, and an item not tied to a type is always eligible.
pub(super) fn nav_item_type_ok(item: &NavItem, product_type: Option<&str>) -> bool {
    match (item.product_types, product_type) {
        (None, _) | (Some(_), None) => true,
        (Some(types), Some(pt)) => types.contains(&pt),
    }
}

pub(super) fn rail_nav(state: &WebState, title: &str, session: Option<&Session>) -> String {
    let base = &state.base_path;
    let role = session
        .map(|s| Role::from_role_str(&s.role))
        .unwrap_or(Role::User);
    let prefs = session
        .filter(|s| Role::from_role_str(&s.role).has_level(Role::Owner))
        .map(|s| super::effective_sidebar_prefs(state, s));

    let active_id = active_nav_id(title);
    // The capability set and product type are view filters only; neither
    // touches a route or page gate.
    let product_type = selected_product_type(state, session);

    // Group order per spec.
    let group_order = [
        "Overview",
        "Offerings",
        "Sales",
        "Fulfill",
        "Communicate",
        "Workspace",
    ];

    let mut out = String::new();
    for group in group_order {
        // One shared visibility predicate for core and capability rows: role,
        // sidebar pref, compile-time feature and runtime product type. Keeping
        // it in one place means a capability row can never bypass a core gate.
        let visible = |item: &NavItem| {
            item.roles.visible(role)
                && if let Some(key) = item.pref {
                    prefs
                        .map(|p| match key {
                            "ai" => p.ai,
                            "crypto" => p.crypto,
                            "admins" => p.admins,
                            "sellers" => p.sellers,
                            "webusers" => p.webusers,
                            _ => true,
                        })
                        .unwrap_or(false)
                } else {
                    true
                }
                && item.feature.is_none_or(capability_enabled)
                && nav_item_type_ok(item, product_type.as_deref())
        };
        // Core rows render first, byte-identical to the shipped table. Each
        // capability row is injected directly after its anchor so "beside AI"
        // holds without touching the 27-row constant.
        let mut items: Vec<&NavItem> = Vec::new();
        for item in NAV_ITEMS.iter().filter(|item| item.group == group) {
            if visible(item) {
                items.push(item);
            }
            // Inject at the anchor's slot even when the anchor itself is
            // pref-hidden, so "beside AI" survives sidebar customisation.
            for extra in FEATURE_NAV_ITEMS
                .iter()
                .filter(|extra| extra.group == group)
                .filter(|extra| {
                    FEATURE_NAV_AFTER
                        .iter()
                        .any(|(id, anchor)| *id == extra.id && *anchor == item.id)
                })
                .filter(|extra| visible(extra))
            {
                items.push(extra);
            }
        }
        if items.is_empty() {
            continue;
        }
        let slug = group.to_ascii_lowercase();
        let current = items.iter().any(|item| Some(item.id) == active_id);
        out.push_str(&format!(
            "<section class=\"nav-group\" data-nav-group=\"{slug}\"{}>",
            if current {
                " data-current=\"true\""
            } else {
                ""
            }
        ));
        out.push_str(&format!(
            r#"<button type="button" class="nav-group-toggle" aria-expanded="true" aria-controls="nav-panel-{slug}"><span class="nav-group-label">{group}</span><svg class="nav-chevron" aria-hidden="true" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5L6 7.5L9 4.5"/></svg></button>"#,
            slug = slug,
            group = group
        ));
        out.push_str(&format!(
            r#"<div class="nav-group-panel" id="nav-panel-{slug}">"#,
            slug = slug
        ));
        for item in &items {
            let active_attr = if Some(item.id) == active_id {
                " class=\"active\" aria-current=\"page\""
            } else {
                ""
            };
            out.push_str(&format!(
                r#"<a href="{base}/{path}"{active}>{label}</a>"#,
                base = base,
                path = item.path,
                active = active_attr,
                label = item.label
            ));
        }
        out.push_str("</div></section>");
    }

    // Utilities strip
    out.push_str(r#"<div class="utilities">"#);
    if session.is_some_and(|s| Role::from_role_str(&s.role).has_level(Role::Owner)) {
        let active_attr = if active_id == Some("settings") {
            " class=\"active\" aria-current=\"page\""
        } else {
            ""
        };
        out.push_str(&format!(
            r#"<a href="{base}/settings"{active}>Settings</a>"#,
            base = base,
            active = active_attr
        ));
    }
    if session.is_some() {
        out.push_str(r#"<button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()" aria-pressed="false" aria-label="Switch to dark theme">Theme</button>"#);
        out.push_str(&format!(
            r#"<a href="{base}/logout">Sign out</a>"#,
            base = base
        ));
    }
    out.push_str("</div>");

    out
}
