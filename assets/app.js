(function () {
  'use strict';
  function init() {
    /** @type {NodeListOf<HTMLElement>} */
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
    /** @type {HTMLElement|null} */
    var vform = document.querySelector('[data-visibility-form]');
    if (vform && !vform.dataset.visBound) {
      vform.dataset.visBound = '1';
      var id = /** @type {HTMLInputElement} */ (vform.querySelector('input[name="channel"]'));
      var vpub = /** @type {HTMLElement} */ (vform.querySelector('#ch-id-help-public'));
      var vpriv = /** @type {HTMLElement} */ (vform.querySelector('#ch-id-help-private'));
      vform.querySelectorAll('input[name="visibility"]').forEach(function (r) {
        var radio = /** @type {HTMLInputElement} */ (r);
        radio.addEventListener('change', function () {
          var priv = radio.value === 'private';
          id.placeholder = /** @type {string} */ (priv ? id.dataset.phPrivate : id.dataset.phPublic);
          vpriv.hidden = !priv;
          vpub.hidden = priv;
          id.setAttribute('aria-describedby', priv ? 'ch-id-help-private' : 'ch-id-help-public');
        });
      });
    }
    // Focus the unlock banner or a re-rendered error alert; the containers
    // carry tabindex="-1", and the page still works without JS.
    /** @type {HTMLElement|null} */
    var banner = document.querySelector('.unlock-banner');
    if (banner && typeof banner.focus === 'function') { banner.focus(); return; }
    /** @type {HTMLElement|null} */
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
    /** @type {NodeListOf<HTMLElement>} */
    var toggles = document.querySelectorAll('.nav-group-toggle');
    toggles.forEach(function (btn) {
      if (btn.dataset.sidebarBound) return;
      btn.dataset.sidebarBound = '1';
      btn.addEventListener('click', function () {
        // The toggle is display:none on mobile; never record a phantom collapse
        // that would resurface as a closed group after returning to desktop.
        if (isMobile()) return;
        var el = /** @type {Element} */ (this.closest('.nav-group'));
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
    var typeSel = /** @type {HTMLSelectElement|null} */ (document.getElementById('scope-type'));
    var prodSel = /** @type {HTMLSelectElement|null} */ (document.getElementById('scope-product'));
    if (!typeSel || !prodSel) return;
    /** @type {HTMLSelectElement} */
    var typeEl = typeSel;
    /** @type {HTMLSelectElement} */
    var prodEl = prodSel;
    function apply() {
      var t = typeEl.value;
      Array.from(prodEl.options).forEach(function (o) {
        // The "All products" reset must always remain reachable.
        if (!o.value || !o.getAttribute('data-type')) return;
        o.hidden = !!t && o.getAttribute('data-type') !== t;
      });
      /** @type {NodeListOf<HTMLOptGroupElement>} */
      var groups = prodEl.querySelectorAll('optgroup');
      if (groups && typeof groups.length === 'number' &&
          (typeof NodeList === 'undefined' || groups instanceof NodeList)) {
        groups.forEach(function (g) {
          if (!g || typeof g.querySelectorAll !== 'function') return;
          var any = false;
          // Each g is an optgroup; only <select> exposes an `options`
          // collection, so enumerate its descendant <option> elements.
          g.querySelectorAll('option').forEach(function (o) { if (!o.hidden) any = true; });
          g.disabled = !!t && !any;
        });
      }
      var chosen = prodEl.options[prodEl.selectedIndex];
      if (t && chosen && chosen.hidden) prodEl.value = '';
    }
    typeEl.addEventListener('change', apply);
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
      var e = /** @type {PerformanceNavigationTiming|undefined} */ (performance.getEntriesByType('navigation')[0]);
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
    try { y = parseInt(/** @type {string} */ (sessionStorage.getItem(KEY)), 10) || 0; }
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
(function () {
  'use strict';
  // Bound from inline `onclick="toggleTheme()"` call sites; a deferred script
  // completes before any interaction, so the global is ready when clicked.
  /** @type {any} */
  var win = window;
  win.toggleTheme = function toggleTheme() {
    var c = document.documentElement.getAttribute('data-theme');
    var n = c === 'dark' ? 'light' : 'dark';
    document.documentElement.setAttribute('data-theme', n);
    localStorage.setItem('theme', n);
    document.querySelectorAll('.theme-toggle').forEach(function (b) {
      b.setAttribute('aria-pressed', n === 'dark' ? 'true' : 'false');
      b.setAttribute('aria-label', n === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
    });
  };
})();
