// Client-side regression harness for assets/app.js (the extracted APP_JS).
// jsdom + node:test smoke tests drive the real DOM hooks; any exception that
// aborts the deferred bundle (e.g. the old `g.options` optgroup access) is
// captured as an uncaught window error and fails every test below.
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { JSDOM } from 'jsdom';

const SOURCE = readFileSync(new URL('../app.js', import.meta.url), 'utf8');

const FIXTURE = `<!DOCTYPE html><html lang="en" data-theme="light"><head></head><body>
<div class="secret-field">
  <button type="button" class="secret-toggle" aria-pressed="false" aria-label="Show password">Reveal</button>
  <input type="password" value="hunter2">
</div>
<div data-visibility-form>
  <input name="channel" data-ph-public="Public" data-ph-private="Private" placeholder="Public">
  <span id="ch-id-help-public"></span>
  <span id="ch-id-help-private" hidden></span>
  <input type="radio" name="visibility" value="public" checked>
  <input type="radio" name="visibility" value="private">
</div>
<section class="nav-group" data-nav-group="overview">
  <button type="button" class="nav-group-toggle" aria-expanded="true">Overview</button>
  <div class="nav-group-panel"><a href="/dashboard">Dashboard</a></div>
</section>
<section class="nav-group" data-nav-group="offerings">
  <button type="button" class="nav-group-toggle" aria-expanded="true">Offerings</button>
  <div class="nav-group-panel"><a href="/products">Products</a></div>
</section>
<div class="rail"></div>
<select id="scope-type">
  <option value="">All types</option>
  <option value="shop_bot">Shop bots</option>
</select>
<select id="scope-product" name="bot_id">
  <option value="">All products</option>
  <optgroup label="Shop bots" data-type="shop_bot">
    <option value="1" data-type="shop_bot">Shop A</option>
  </optgroup>
  <optgroup label="Movie bots" data-type="movie_bot">
    <option value="2" data-type="movie_bot">Movie A</option>
  </optgroup>
</select>
<button type="button" class="theme-toggle" aria-pressed="false" aria-label="Switch to dark theme">Theme</button>
<div class="alert" data-focus-on-load tabindex="-1"></div>
</body></html>`;

function boot() {
  const dom = new JSDOM(FIXTURE, {
    url: 'http://localhost/dashboard?x=1',
    runScripts: 'outside-only',
  });
  const { window } = dom;
  const { document } = window;
  const errors = [];

  window.addEventListener('error', (ev) => errors.push(ev.error || ev.message));
  window.addEventListener('unhandledrejection', (ev) => errors.push(ev.reason));

  // jsdom implements neither matchMedia nor scrolling. Desktop answers make
  // the sidebar and rail memory paths reachable; mobile answers stay false.
  window.matchMedia = (query) => ({
    media: query,
    matches: /min-width:\s*901px/.test(query),
    onchange: null,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
    dispatchEvent() { return false; },
  });
  window.scrollTo = () => {};
  window.requestAnimationFrame = (cb) => { cb(); return 1; };

  // A <select> has an `options` collection; an <optgroup> does not. The old
  // crash iterated `g.options`, so make that property actively hostile.
  Object.defineProperty(window.HTMLOptGroupElement.prototype, 'options', {
    configurable: true,
    get() { throw new Error('HTMLOptGroupElement.options must never be read'); },
  });

  // Force the deferred-script fast path so all six IIFEs initialise now and
  // never register a DOMContentLoaded listener (jsdom fires its own later).
  Object.defineProperty(document, 'readyState', { value: 'complete', configurable: true });

  try {
    window.eval(SOURCE);
  } catch (err) {
    errors.push(err);
  }
  return { dom, window, document, errors };
}

function assertClean(errors) {
  assert.deepEqual(errors, [], 'app.js raised an uncaught window error');
}

test('deferred bundle installs the toggleTheme global with zero uncaught errors', () => {
  const { window, errors } = boot();
  assertClean(errors);
  assert.equal(typeof window.toggleTheme, 'function', 'toggleTheme must be a window global');
  assert.ok(window.document.querySelector('.secret-toggle'));
  assert.ok(window.document.querySelector('.nav-group'));
  assert.ok(window.document.querySelector('#scope-product optgroup'));
  assert.ok(window.document.querySelector('.theme-toggle'));
});

test('secret reveal swaps the input type and keeps the typed value', () => {
  const { window, document, errors } = boot();
  const btn = document.querySelector('.secret-toggle');
  const input = document.querySelector('.secret-field input');
  assert.equal(input.type, 'password');
  btn.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(input.type, 'text');
  assert.equal(input.value, 'hunter2');
  assert.equal(btn.getAttribute('aria-pressed'), 'true');
  btn.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(input.type, 'password');
  assert.equal(btn.getAttribute('aria-pressed'), 'false');
  assertClean(errors);
});

test('type filter disables empty optgroups without touching g.options', () => {
  const { window, document, errors } = boot();
  const typeSel = document.getElementById('scope-type');
  const shop = document.querySelector('optgroup[data-type="shop_bot"]');
  const movie = document.querySelector('optgroup[data-type="movie_bot"]');
  assert.equal(shop.disabled, false);
  assert.equal(movie.disabled, false);
  typeSel.value = 'shop_bot';
  typeSel.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(shop.disabled, false, 'a group with a visible option stays enabled');
  assert.equal(movie.disabled, true, 'a group with only hidden options is disabled');
  assert.equal(document.querySelector('option[value="2"]').hidden, true);
  assertClean(errors);
});

test('theme global toggles the theme and syncs every theme control', () => {
  const { window, document, errors } = boot();
  const btn = document.querySelector('.theme-toggle');
  assert.equal(document.documentElement.getAttribute('data-theme'), 'light');
  assert.equal(btn.getAttribute('aria-pressed'), 'false');
  window.toggleTheme();
  assert.equal(document.documentElement.getAttribute('data-theme'), 'dark');
  assert.equal(btn.getAttribute('aria-pressed'), 'true');
  assert.equal(btn.getAttribute('aria-label'), 'Switch to light theme');
  assert.equal(window.localStorage.getItem('theme'), 'dark');
  window.toggleTheme();
  assert.equal(document.documentElement.getAttribute('data-theme'), 'light');
  assert.equal(btn.getAttribute('aria-pressed'), 'false');
  assertClean(errors);
});

test('collapsed nav groups persist their open state to localStorage', () => {
  const { window, document, errors } = boot();
  const group = document.querySelector('.nav-group[data-nav-group="overview"]');
  const btn = group.querySelector('.nav-group-toggle');
  const panel = group.querySelector('.nav-group-panel');
  assert.equal(group.classList.contains('is-collapsed'), false);
  assert.equal(panel.hasAttribute('inert'), false);
  btn.dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(group.classList.contains('is-collapsed'), true);
  assert.equal(btn.getAttribute('aria-expanded'), 'false');
  assert.equal(panel.hasAttribute('inert'), true);
  assert.equal(panel.getAttribute('aria-hidden'), 'true');
  const state = JSON.parse(window.localStorage.getItem('tg.sidebar.groups.v1'));
  assert.equal(state.overview, false, 'stored value is the open flag (false = collapsed)');
  assertClean(errors);
});

test('pagehide persists window and desktop rail scroll offsets', () => {
  const { window, document, errors } = boot();
  Object.defineProperty(window, 'scrollY', { value: 128, configurable: true });
  const rail = document.querySelector('.rail');
  Object.defineProperty(rail, 'scrollHeight', { value: 900, configurable: true });
  Object.defineProperty(rail, 'clientHeight', { value: 300, configurable: true });
  Object.defineProperty(rail, 'scrollTop', { value: 456, writable: true, configurable: true });
  window.dispatchEvent(new window.Event('pagehide'));
  const pages = JSON.parse(window.sessionStorage.getItem('tg.scroll.v1'));
  assert.equal(pages['/dashboard?x=1'], 128);
  assert.equal(window.sessionStorage.getItem('tg.rail.scroll.v1'), '456');
  assertClean(errors);
});
