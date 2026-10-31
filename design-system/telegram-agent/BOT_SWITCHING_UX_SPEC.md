# Bot Switching — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec of record for the shipped surface; L7 implemented in `src/web.rs`.
**Status:** v1.1 spec (2026-10-28): remaining console pages scoped per N1; no new controls. WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L10 (security review), L6-C (token contract).
**Related:** `design-system/telegram-agent/MASTER.md` (token contract), `assets/app.css` (`.rail .bot-scope`), `src/web.rs` (`html_page` shell, `bot_scope_control`, `bot_select`, `bots_page`/`bots_save`, `effective_bot`), `AUTO_ROUTE.md` L7 plan (`web-bot-scoping-plan.md`).

---

## 0. Summary of decisions

1. **One console, one login, bot scope in the rail.** No per-bot ports or logins. The global owner switches scope from a selector in the sidebar; everyone else sees a pinned label naming the bot that already resolves their view server-side.
2. **The control is a real form, not a JS-only widget.** `<select onchange="this.form.submit()">` is progressive enhancement; a visible **Switch** button keeps the flow working without JavaScript and gives keyboard users a non-trapping submit path.
3. **Selection persists against the session row** (`web_sessions.selected_bot_id`), never a cookie or query value; a forged `bot_id` cannot widen a pinned session (server rejects with 403).
4. **The Bots page edits metadata, never tokens.** Tokens live in `BOTS_CONFIG`/`BOTS_JSON`/env and a restart; the page says so in its help copy.
5. **No new tokens.** Built from existing primitives (`rail`, `group`, `mono`, `btn btn-ghost btn-sm`, `table-wrap`, `panel`, `help`, `badge`) — see §6.

---

## 1. Bot scope control (rail, every shell page)

### Placement and structure

Sits in `<aside class="rail">` directly under the brand block and above the `<nav>`, so scope is the first thing read after identity on every page. Two variants, decided server-side in `bot_scope_control`:

| Who | Variant | Markup |
|---|---|---|
| Global owner (`role == "owner"`) | Selector | `<form method="post" action="{base}/bot/select" class="bot-scope"><label for="bot-scope-select">Bot</label><select …>…</select><button type="submit" class="btn btn-ghost btn-sm">Switch</button></form>` |
| Seller / admin | Pinned label | `<div class="bot-scope bot-scope-pinned" aria-label="Active bot"><span class="help">Bot</span><span class="mono">{key}</span></div>` |
| Pre-auth / other | (absent) | empty string |

### Options (owner variant)

1. `All bots (primary)` — `value=""`, selected when `selected_bot_id` is `None`. Resets to the unscoped global view, not to a filter on the literal primary bot (§5).
2. One option per **enabled** bot (`list_bots_enabled()`), labeled `key · @username` (username segment omitted when unset), `value="{bot_id}"`, `selected` when it matches the session. Disabled bots are excluded here **and** rejected server-side in `bot_select` — the picker can never offer a state the server refuses.

### Behavior

- `onchange` submits the form immediately (mouse/pointer convenience). The submit button remains rendered and focusable — keyboard users tab to it and press Enter; nothing depends on JS.
- POST → validate → persist → **PRG redirect**: back to the Referer's path when it is a console-local path under the configured base path (scheme/host discarded), else `{base}/dashboard`.
- The next full page render reflects the new scope in the selector's preselected option and in bot-scoped page data (plans, products, orders, channels and the remaining console pages — see §5).

### State change feedback

The PRG target re-renders with the new preselection — that is the v1 acknowledgment. **Follow-up (non-blocking):** a `role="status"` live region announcing "Now operating as {key}" would make the change explicit for screen-reader users without a page reload announcement; deferred because the full reload already re-announces the document.

---

## 2. Pinned label (sellers and admins)

- Plain text, `aria-label="Active bot"`, showing the resolved bot **key** via `effective_bot()` (seller → `for_seller`, admin → `admins.bot_id`, fallback primary).
- Must never look like a disabled control: no fake-select chrome, no `disabled` attribute, no cursor hint. It is a fact, not a widget.
- A switch attempt by a pinned role is **rejected server-side** (403, `forbidden_page`); the UI never offers the action in the first place.

---

## 3. Bots management page (`/bots`, global owner only)

- **Read-only table** (`.table-wrap` region): ID · Key · Username · Owner · Default channel · Enabled. Empty state points to `BOTS_CONFIG`/`BOTS_JSON` + restart.
- **One "Edit bot" form** (`.panel`): bot `<select>` (all bots), owner user ID (number, required), default channel ID (number, optional), enabled checkbox (pre-checked), Save → POST `{base}/bots`.
- Unknown bot id → styled 404 page, never a silent redirect (honest feedback).
- Token editing is **out of scope by design**; the help copy under the form title says so and names the config path. No password-style field may be added here (L10 floor).
- Rail entry lives in the **Operate** group.

---

## 4. Navigation grouping & breadcrumb (shipped state)

- `rail_nav` groups entries under `.group` headings: **Operate** (Dashboard, Channels, Plans, Products) · **Sell** (Codes, Promos, Referrals, Orders, Finance, Customers, Cart) · **Fulfill** (Subscriptions, Bookings, Tickets) · **Insights** (Analytics, Posts, Broadcast, Keys, Carts, Settings, Log out).
- Group headings are non-interactive text — they must stay out of the tab order (they are plain `<div>`s, not `<button>`/`<details>`).
- Breadcrumb: `<nav aria-label="Breadcrumb">{page title}</nav>` directly above the `<h1>` — single level today (group > page is a v2 candidate; adding it must not duplicate the visible `<h1>` text for screen readers).

---

## 5. Bot-scoped data (what the selector changes)

| Surface | Scoped? | Rule |
|---|---|---|
| Plans (`/plans`) | yes | rows whose `seller_id` ∈ `sellers_for_bot(selected)` |
| Products (`/products`) | yes | same seller-set rule |
| Orders (`/orders`) | yes | same seller-set rule |
| Channels (`/channels`) | yes | rows whose channel resolves to the selected bot (`for_channel`) |
| Dashboard (`/dashboard`) | yes | plans and recent subscriptions filtered by the selected bot's seller set; the default-channel readout resolves the SELECTED bot's default channel (fallback `(none)`), and the global config value when `All bots (primary)` is selected |
| Codes (`/codes`) | yes | code rows AND the plan picker filtered by the selected bot's seller set |
| Reviews (`/reviews`) | yes | products (and therefore their pending reviews) filtered by the selected bot's seller set |
| Analytics (`/analytics`) | yes | aggregate seller set = the selected bot's sellers; `All bots (primary)` keeps today's global aggregate |
| Finance (`/finance`) | yes | payout and refund lists filtered by the selected bot's seller set; the create forms still use the owner's global privilege — scoping is a VIEW filter, not a permission change |
| Sends (broadcast, invite links, `/setupbot` profile) | yes | `effective_bot()` — operates **as** the selected bot |

Sellers and admins are unaffected: their views stay pinned/scoped as in §2. Every write/POST action keeps its existing server-side authorization — the selector only narrows what a scoped owner reads.

### "All bots (primary)" semantics

`All bots (primary)` (`value=""`, selected when `selected_bot_id` is `None`) is the unscoped global view and the reset state, not a filter to the literal primary bot. Selecting a bot narrows each surface per the table above; clearing the selection widens every surface back to all sellers, the global aggregates and the global default channel.

---

## 6. WCAG 2.1 AA floor (non-waivable)

- **Labels:** the select has a programmatic `<label for="bot-scope-select">`; the Bots form fields are labeled; the pinned label is announced via `aria-label` on its container.
- **Keyboard:** select and Switch button are native controls — tab reachable, Enter/Space actuate; `onchange`-submit cannot trap focus because the button is always present.
- **Focus visibility:** native focus ring per MASTER token contract (`--ring`); no `outline: none` anywhere on the new controls.
- **Contrast:** `.rail .bot-scope label` / `.help` use `--ink-3` at 11.5px 600 — verified ≥ 4.5:1 against `--surface` in both themes; the pinned `.mono` value uses `--ink`.
- **Semantics:** the pinned label is plain text (no widget role); the selector is a native `<select>` with a real form action; page-level changes are full navigations (PRG), so screen readers re-announce the document naturally.
- **Motion/color independence:** scope state is conveyed by text (selected option, pinned key), never by color alone.
- **No new surfaces:** this change introduces no new controls, so the keyboard, focus, label and contrast floors above are unchanged; a filtered page that comes back empty reuses that page's existing empty copy rather than new messaging.

---

## 7. Known follow-ups (non-blocking)

**Shipped (2026-10-28):** seller-set scoping extended to Dashboard, Codes, Reviews, Analytics and Finance (§5) — no new controls, tokens or copy rode along.

1. Live-region confirmation of a switch (§1).
2. Breadcrumb gains a group level (§4).
3. Bot-scoped admins need a future console surface: `Db::set_admin_bot_id` now exists in the data layer, but no console path calls it (v2). Until then an admin's pinned label resolves `admins.bot_id` when set and the primary bot otherwise.
