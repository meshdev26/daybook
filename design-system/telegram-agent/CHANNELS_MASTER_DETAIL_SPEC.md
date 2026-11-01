# Channels Master–Detail — Dedicated Add Page, Read-First Master List, Per-Channel Manage View

**Status:** Approved, ready for L7 implementation
**Date:** 2026-11-01
**Role:** L6 senior-design-ux
**Territory of this spec:** the Channels page structure — a dedicated
`GET /channels/add` page, a read-first master list, and a per-channel
`GET /channels/manage` detail view. Code changes land in
`src/web/channels.rs`, `src/web/channels_rows.rs`, a NEW
`src/web/channels_manage.rs`, `src/web/mod.rs` (route + module wiring), ONE
additive rule in `assets/app.css`, and `src/web/tests/channels.rs` +
`src/web/identity_tests.rs` (pin retargeting). Nothing else.
**Sources of truth:** `DESIGN.md` + `assets/app.css` (MASTER.md is
deprecated 2026-10-31 and must not be used). Specs this one builds on and
must not disturb: `CHANNEL_ADD_REDESIGN_SPEC.md` (visibility-aware add, the
post-add reveal + suppression matrix, the name/address split),
`CHANNELS_TITLE_UX_SPEC.md` (label contract), `MANAGEMENT_DISCLOSURE_SPEC.md`
§3 (scoped-empty honesty), `IDENTITY_SENDER_UX_SPEC.md` (binding gates +
badge placement).
**Hard constraints:** move-only-where-possible — every POST route, every
gate, every Db call, and the reveal/callout semantics stay byte-identical in
behavior; only redirect targets and page placement change. No DB changes. No
new POST routes. Every dynamic string `esc()`-ed; every href/action through
`state.url()` (base-path safe; all test routers run `base_path: ""` —
`test_support.rs:46`, `tests/mod.rs:54` — so Location pins are literal).

---

## 0. Owner mandate and the structural answer

This is the third ask in different words. Today every master-list row carries
its full edit cluster — Post as user / Set default / name input + Edit name /
Change address disclosure / Delete — and the add form stacks under the list
(`channel_row_actions_owner`, `channels_rows.rs:12-82`;
`channel_row_name_controls`, `channels_rows.rs:90-130`; the owner add form,
`channels.rs:240-268`). The owner calls it "too complicated / not clean
minimalism". The answer is structural, not copy polish:

1. **Add gets its own page** — `GET /channels/add`.
2. **The list becomes a master list** — read-first rows, state badges only,
   exactly ONE affordance per row: `Manage →`.
3. **Relative things move to a per-channel detail view** —
   `GET /channels/manage?channel=…` hosts Name, Address, Default, Posting
   as, and an isolated Danger zone.

Design principles exercised (DESIGN.md §1, §3): one primary action per
screen, progressive disclosure, no competing controls inside a row; the
operator's-ledger calm (borders lead, nothing loud).

---

## 1. Verified current state (file:line)

| Fact | Location |
|---|---|
| `GET /channels` page handler → `channels_body` | `src/web/channels.rs:3-25`, `:184-199` |
| PRG banner factory + fixed `?error=` copy | `channels_banner` `channels.rs:30-37`, `channels_error_copy` `:43-78`, `channels_banners` `:82-152` |
| `ChannelsQuery` — all flash fields | `channels.rs:161-176` |
| Owner global render: scoped filter, empty states, callout, add form | `render_global_channels` `channels.rs:203-287` (bot-scope filter `:224`, scoped-empty `:228-229`, empty `:231`, add form `:240-268`, callout slot `:271-273`, footer help `:279`) |
| Inline-error re-render (list page, 200) | `channels_error` `channels.rs:291-299` |
| `POST /channels/add` handler: resolve, PRG keys, seller branch | `channels.rs:360-444` (empty guard `:371-373`, shape errors `:395-400`, `added_q` `:413-420`, seller set `:423-430`, owner add + title `:431-440`, admin forbidden `:441-443`) |
| `POST /channels/default` — owner-only 403 | `channels.rs:446-469` (gate `:456-458`, PRG `:466`) |
| `POST /channels/remove` — owner global / seller own; default protected | `channels.rs:471-512` (owner `:489-500`, seller `:501-508`, default refusal copy `:490-496`) |
| `POST /channels/rename` — seller-allowed API, NOCASE guard | `channels.rs:554-619` (seller sid `:564-572`, guard `:604-606`, PRG `:611-615`, DB-err redirect `:616-618`) |
| `POST /channels/name` — owner-only 403, global rows only | `channels.rs:634-677` (gate `:644-646`, existence probe `:653-659`) |
| `POST /channels/bind-sender` — owner-only 403, gate+readiness | `channels.rs:697-783` (gate `:707-709`, shape check `:714-720`, global-only `:726-735`, posting/locked/identity redirects `:739-753`, PRG `:778-782`) |
| Row action cluster (bind/default/name/delete/hint) | `channel_row_actions_owner` `channels_rows.rs:12-82` (default hint `:76-80`) |
| Name controls (Edit name + Change address) | `channel_row_name_controls` `channels_rows.rs:90-130` (prefill `:103-105`, shape warning `:114-118`) |
| Row builder + reveal attrs | `render_global_channel_row` `channels_rows.rs:137-186` (`row-added` `:172-176`, label `:163`, notitle hint `:167-171`) |
| Post-add callout + focus anchor | `channel_added_callout` `channels_rows.rs:194-214`, `added_focus` `:261-264`, `channels_added_callout` `:270-298` |
| Badge + label helpers (render-derived, kept) | `channel_type_badge` `channels_rows.rs:223-229`, `channel_label` `:235-240` |
| Seller view: one row + Delete + add form | `render_seller_channels` `channels_rows.rs:307-387` (row + Delete `:343-345`, empty `:354`, form `:361-378`, notitle hint `:327-331`) |
| Routes: `/channels`, `/channels/add` (POST only — GET 405s today) | `src/web/mod.rs:245-246`; `routes_channels` `:253-258` (default/name/remove/rename/bind-sender) |
| Bot API ops routes (`/channels/title` etc.) — OUT OF SCOPE | `src/web/mod.rs:363-365` |
| Bot-scope resolution | `Bots::for_channel` `src/bots.rs:96-109` (channel bot → seller bot → primary) |
| APP_JS `data-visibility-form` binding is page-generic | `src/web/html.rs:22-44` (binds ANY `[data-visibility-form]` on any page) |
| `data-focus-on-load` focuses first match, unlock banner wins | `src/web/html.rs:46-53` |
| Dedicated-add-page precedent (authenticated, rail shell, h1+panels) | `render_add_page`/`bots_add_page` `src/web/bots_add.rs:294-399` |
| Focused inline error alert precedent | `focused_error_alert` `src/web/identity.rs:241`; used by `bot_add_error_page` `src/web/bots_add.rs:263-273` |
| `.door` shell is the setup/login front door only | `src/web/html.rs:261` (`<main class="door" id="main">`), CSS `assets/app.css:206-223` |
| House CSS: `.page-head` + `.page-head .actions` (unused today — ready) | `assets/app.css:200-203` |
| House CSS: row list flex, `.row-actions`, `.row-added` tint | `assets/app.css:420-440`, `:316` |
| House CSS: `.btn` = primary (brand fill), `.btn-secondary/-danger/-ghost/-sm` | `assets/app.css:388-416` |
| House CSS: `.panel`, `.help`, `.help-more`, `.mono`, `.badge*`, `.alert*` | `assets/app.css:228-235`, `:345`, `:348-350`, `:442`, `:478-491`, `:495-511` |
| Cap rule (target ≤800; at cap the next change splits first) | `AUTO_ROUTE.md:159` |
| Helpers: `esc`, `js_arg`, `urlencode_query`, `forbidden_page` | `src/web/html.rs:124,128`, `src/web/common_pages.rs:21` |

Test files scanned for pins: `src/web/tests/channels.rs` (682),
`src/web/identity_tests.rs` (channel-sender block `:949-1165` + dashboard
composer `:1168+`), `src/web/smoke_tests.rs` (`:168-183`, `:268-288`,
`:508-509`), `src/web/tests/misc.rs`, `tests/bots.rs`, `tests/settings.rs`,
`tests/setup.rs`, `tests/subs.rs`, `timeout_tests.rs`,
`security_tests.rs`. **misc/bots/settings/setup/subs/timeout/security
contain zero Channels-page markup pins** (verified by grep — their only
channel touches are DB calls and Bot API authz probes, untouched here).

---

## 2. Route plan (old vs new)

| Route | Before | After |
|---|---|---|
| `GET /channels` | list + inline edit clusters + add form stacked under list | **master list**: page-head with one `Add channel` primary button (top-right), read-first rows, `Manage →` per row, empty states, reveal preserved |
| `GET /channels/add` | **did not exist** (route is POST-only, `mod.rs:246` — a GET 405s) | **NEW dedicated add page** (owner and seller variants; admin → `forbidden_page`, matching the POST gate `channels.rs:441-443`) |
| `POST /channels/add` | resolve + PRG to `/channels?…` | **handler unchanged**; success PRG unchanged (`/channels?added=1[&notitle=1][&channel=X]`); shape/empty error PRGs retarget to `/channels/add?error=…`; DB-Err inline re-render moves from list to add page |
| `GET /channels/manage?channel=X` | did not exist | **NEW per-channel detail view** (owner: five sections; admin: read-only facts; seller: own-channel facts + Danger zone) |
| `POST /channels/default` | PRG `/channels?default=1` | **handler + gate unchanged**; PRG → `/channels/manage?channel=X&default=1` |
| `POST /channels/name` | PRG `/channels?named=1&channel=X` | **unchanged**; PRG → `/channels/manage?channel=X&named=1` |
| `POST /channels/rename` | PRG `/channels?renamed=1…` / `/channels?error={e}` | **unchanged**; PRG → `/channels/manage?channel=X&renamed=1[&same=1]`; DB-err PRG → `/channels/manage?channel={old}&error={e}` |
| `POST /channels/bind-sender` | PRG `/channels?bound-*=1&channel=X`, `/channels?error=posting|locked|identity` | **unchanged**; PRGs → `/channels/manage?channel=X&…` |
| `POST /channels/remove` | PRG `/channels?removed=1` (both roles) | **byte-identical** — the target row is gone after removal, so the outcome is list-level |
| Scripted-only `error=empty` guards (default/remove/name/rename/bind-sender) | `/channels?error=empty` | **byte-identical** (no UI path produces them — every form carries its channel) |
| Inline 200 errors (`channels_error`) | re-render list page | **byte-identical** — every remaining inline path is scripted-only (see §6); the one UI-reachable inline error (add DB Err) moves to the add page |
| `/channels/invite`, `/channels/title`, `/channels/desc` (Bot API ops) | unchanged | unchanged — different feature, different page (`channel_ops.rs`) |

### mod.rs wiring

```rust
// mod.rs:35-36
mod channels;
mod channels_rows;
mod channels_manage;          // NEW
// mod.rs:80-81
use channels::*;
use channels_rows::*;
use channels_manage::*;       // NEW

// mod.rs:246 — GET joins the existing POST on one line
.route("/channels/add", get(channels_add_page).post(channels_add))

// routes_channels (mod.rs:253-258) gains:
r.route("/channels/manage", get(channels_manage_page))
```

Zero new POST routes; all five action routes keep their field names
(`channel`, `identity`, `name`, `new`) so scripted posters keep working
unchanged.

---

## 3. `GET /channels/add` — the dedicated add page

**Idiom: panel page, not the `.door` shell.** The door (`html.rs:261`,
`app.css:206-223`) is the setup/login front door — an unauthenticated shell.
`/channels/add` is an authenticated console page; the house precedent for a
dedicated add page inside the rail is the Add-bot wizard
(`bots_add.rs:294-399`): `html_page` + `h1` + panel. The form itself is a
byte-verbatim MOVE of the form blocks prescribed by
CHANNEL_ADD_REDESIGN_SPEC §3.1/§3.2 — no field, id, help, or copy changes.

### 3.1 Page skeleton (owner)

```html
<h1>Add channel <span class="sub">new posting target</span></h1>
{banner — one channels_banner("error", …) when ?error= is present}
<article class="panel">
  <form method="post" action="{url}/channels/add" data-visibility-form>
    <label for="ch-title">Title</label>
    <input id="ch-title" name="title" maxlength="64" autocomplete="off" placeholder="e.g. Wire Guard">
    <span class="help">Your label for this channel — how it reads in lists and pickers. Leave empty and I'll fill it from Telegram when the bot can see the channel.</span>

    <fieldset class="visibility">…public/private radio, byte-verbatim…</fieldset>

    <label for="ch-id">Channel</label>
    <input id="ch-id" name="channel" required … data-ph-public data-ph-private … aria-describedby="ch-id-help-public">
    <span class="help" id="ch-id-help-public">…</span>
    <span class="help" id="ch-id-help-private" hidden>…</span>

    <details class="help-more"><summary>Which forms are accepted?</summary>…kept tail: "Adding one does not change the default — use Set default."</details>

    <button type="submit">Add channel</button>
  </form>
  <p><a href="{url}/channels">Back to channels</a></p>
</article>
```

- The pinned help-more strings move with the form verbatim
  (`<details class="help-more"><summary>Which forms are accepted?</summary>`,
  `Adding one does not change the default — use Set default.`) — they
  describe semantics, and the pin (`tests/channels.rs:186,190`) survives on
  the new page.
- APP_JS needs **zero changes**: the `data-visibility-form` binder is
  page-generic (`html.rs:22-44`).

### 3.2 Seller variant (role-branched in the same builder)

- NO Title field (honesty: `set_channel_title` only matches `channels` rows;
  seller channels live in `sellers.default_channel` — CHANNEL_ADD_REDESIGN
  SPEC §3.2; pinned negative `tests/channels.rs:217` survives).
- help-more tail verbatim: `Adding one replaces the previous.`
- Submit button verbatim: `Set channel`.
- h1 sub: `your posting target`.

### 3.3 Gating and errors

- Owner and seller → 200. Admin/user → `forbidden_page(&state, "Channels", "Add channel")` (mirror of the POST's `channels.rs:441-443`).
- Unauthenticated → `Redirect::to(&state.url("/login"))`.
- Shape/empty refusals arrive as PRG `?error=…` and render through the
  existing `channels_banner("error", …)` + `channels_error_copy` (fixed keys;
  unknown values escaped — `channels.rs:43-78`). `role="alert"` announces
  assertively.
- The DB-Err inline path (duplicate NOCASE add, `channels.rs:439`) re-renders
  the ADD page with `focused_error_alert` (`identity.rs:241`; precedent
  `bot_add_error_page`, `bots_add.rs:263-273`) — the failed action's message
  belongs on the page it failed from. ≈10-line helper `channels_add_error`.
- Nav: pass title `"Channels"` to `html_page` so the rail keeps
  `aria-current="page"` on Channels (`nav_link`, `html.rs:276-283` matches by
  title).

---

## 4. The master list (`GET /channels`)

### 4.1 Page head — one primary affordance

```html
<div class="page-head">
  <h1>Channels <span class="sub">the global posting targets</span></h1>
  <div class="actions"><a class="btn" href="{url}/channels/add">Add channel</a></div>
</div>
<article class="panel"><h3>Global channels</h3><ul>…rows…</ul>
{callout — unchanged slot, channels.rs:271-273}
<p class="help">Sellers manage their own channel via /setchannel in the bot.</p>
</article>
```

- `.page-head`/`.page-head .actions` already exist (`app.css:200-203`) and
  are currently unused — first consumer, zero new CSS. `a.btn` with no
  modifier IS the primary button (brand fill, `app.css:388-404`).
- Sellers get the same page-head with the same `Add channel` link (their
  POST path = set/replace). **Admins get NO button** — they have no add
  permission (POST gate `channels.rs:441-443`), same honesty as today's
  missing form (`channels.rs:240`).

### 4.2 Read-first rows

Each row: label cluster + state badges + exactly ONE affordance.

```html
<li>
  <span class="mono">Wire Guard (@wire)</span> <span class="badge">public</span><span class="badge badge-success">default</span><span class="badge badge-warning">as user</span><span class="help">no title yet — open Manage to name it</span>
  <div class="row-actions"><a class="btn btn-secondary btn-sm" href="{url}/channels/manage?channel=%40wire" aria-label="Manage Wire Guard">Manage <span aria-hidden="true">→</span></a></div>
</li>
```

- DOM skeleton identical to today (`li > span.mono + badges + .row-actions`)
  so the flex row styles (`app.css:420-440`) and the `.row-added` tint
  (`app.css:316`) keep working unchanged.
- Badges kept byte-verbatim from `channel_type_badge` / default / `as user`
  (`channels_rows.rs:143-158`): badges are STATE, not affordances — the
  pinned exception-only marking (`identity_tests.rs:1143`, exactly one
  `>as user</span>`) survives on the master list.
- `channel_label` unchanged (`channels_rows.rs:235-240`) — "Title (id)" or
  bare identifier.
- The default-row hint (`Set another channel as default to remove`,
  `channels_rows.rs:76-80`) MOVES to the detail Default section (§5.4).
- The notitle hint copy re-points: `no title yet — use Edit name to name it`
  → `no title yet — open Manage to name it` (unpinned; verified — no test
  contains "no title yet").
- **Accessible name** per row: `aria-label="Manage {title-or-identifier}"`
  — the stored title when present ("Manage Wire Guard"), else the
  identifier ("Manage @wire" / "Manage -1001234567890123"). The visible
  text is `Manage` + an `aria-hidden` arrow.
- NO inline forms remain in rows: the whole of
  `channel_row_actions_owner` + `channel_row_name_controls` leaves the row
  family (§9).

### 4.3 Empty states (kept honest, one word fixed)

- Genuinely empty: `<b>No global channels yet.</b> Add one — it becomes a
  posting target for plans and posts.` — today's copy (`channels.rs:231`)
  minus the word "below" (the form is no longer below; the pinned prefix
  `No global channels yet.`, `tests/channels.rs:149`, survives).
- Scoped artifact (bot selected, rows resolve elsewhere): byte-verbatim
  `No channels resolve to this bot yet. …switch the Bot selector above to All bots.` (`channels.rs:229`; pin `tests/channels.rs:156` survives).
- Seller empty: `No channel yet. Add one, or use /setchannel in the bot.`
  (drops "below"; unpinned).

### 4.4 Post-add reveal — preserved byte-identically

The anchor → highlight → callout → suppression machinery is untouched:

- Redirect `?added=1&channel=X[&notitle=1]` (owner and seller) lands on the
  LIST; the NOCASE-matching row renders `<li class="row-added"
  tabindex="-1" data-focus-on-load>` (`channels_rows.rs:172-176`); APP_JS
  focuses it (`html.rs:46-53`); the in-panel callout renders after the
  `<ul>` (`channels_rows.rs:270-298`); the top add banner is suppressed
  (`channels.rs:86-111`). Legacy `?added=1` keeps today's banners.
- Two callout/banner tails re-point at the detail view (unpinned copies):
  - owner callout tail: ` — add it in Telegram, then open Manage and set it
    default if this is your main channel.` (was "then use Set default…");
    notitle label hint: ` Open Manage to give it a label.` (was "Use Edit
    name…").
  - legacy notitle banner tail (`channels.rs:100`): ` Use Edit name to name
    it.` → ` Open Manage to name it.`
- This is the "relative things" moment: add on its own page, come back to a
  clean list with the new member highlighted and named.

---

## 5. `GET /channels/manage?channel=X` — the detail view

### 5.1 Layout

```
┌──────────────────────────────────────────────────────────────────┐
│ Back to channels                                                  │
│                                                                   │
│ Wire Guard (@wire)                         ← h1 = channel_label   │
│ ●public ●default ●as user                 ← state badge row      │
│ {action banners — channels_banners(&q, …)}                        │
│                                                                   │
│ ┌ Identity ────────────────────────────────────────────────────┐ │
│ │ Title                                                        │ │
│ │ [ Tech Tips                ]  (Save name)                    │ │
│ │ help: Your label for this channel — never the dialed address.│ │
│ └──────────────────────────────────────────────────────────────┘ │
│ ┌ Address ─────────────────────────────────────────────────────┐ │
│ │ @wire  (mono, read-only)                                     │ │
│ │ ▸ Change address…                                            │ │
│ │   [input prefilled w/ current address] (Retarget — danger)   │ │
│ └──────────────────────────────────────────────────────────────┘ │
│ ┌ Default ─────────────────────────────────────────────────────┐ │
│ │ (Set default)  — or —  This is the default channel.          │ │
│ │                        Set another channel as default to     │ │
│ │                        remove this one.                      │ │
│ └──────────────────────────────────────────────────────────────┘ │
│ ┌ Posting as ──────────────────────────────────────────────────┐ │
│ │ (Post as user…)  — or —  (Post as bot)                       │ │
│ └──────────────────────────────────────────────────────────────┘ │
│ ┌ Danger zone ─────────────────────────────────────────────────┐ │
│ │ Removing disconnects every plan and post that dials this     │ │
│ │ address. This cannot be undone.                              │ │
│ │ (Delete)   — default rows: "The default channel cannot be    │ │
│ │              removed — set another channel as default first."│ │
│ └──────────────────────────────────────────────────────────────┘ │
└──────────────────────────────────────────────────────────────────┘
```

Heading order: `h1` (channel label) → `h2` per section — no skipped levels.
Sections are `article.panel` blocks; Danger zone is the last panel, visually
flagged by the one new CSS rule (§8). Back link is a plain
`<p><a href="{url}/channels">Back to channels</a></p>` (house idiom:
`channel_ops.rs:222`, `bots_add.rs:284`).

### 5.2 Resolution and refusals (fail closed, scoped-empty register)

- **Owner:** find the row in `list_channels(None)` with `seller_id == 0`
  and NOCASE `channel == X` (mirrors the list filter, `channels.rs:222-224`).
- **Bot scope:** if `session.selected_bot_id` is `Some(b)` and
  `state.bots.for_channel(&row.channel).id != b` — render the refusal page:
  `alert-error`: **`This channel belongs to another bot — switch the Bot
  selector to All bots to manage it.`** + Back link, 200. Same family as the
  list's scoped-empty copy (`channels.rs:229`); no channel data leaks.
  (The Manage link only renders on in-scope rows, so this path is
  hand-typed URLs — it must still fail honest.)
- **Unknown / seller-owned row for an owner:** `alert-error`: **`That
  channel isn't in the global list — add it first.`** (verbatim reuse of the
  `channels_name` refusal copy, `channels.rs:656-658`) + Back link.
- **Missing `?channel=`:** redirect to `/channels`.
- **Seller:** resolve own channel via `seller_channel(sid)` NOCASE-matched
  against X (the legacy channel may have no `channels` row —
  `channels_rows.rs:314-331` reads the title separately). Any other X →
  `alert-error`: **`That channel isn't yours to manage.`** + Back link.
- **Admin:** same resolution as owner; read-only page (§5.7).

### 5.3 Identity (owner)

The Edit-name control moves from the row to this section — the form posts
the same fields to the same route:

```html
<form method="post" action="{url}/channels/name" class="inline-form">
  <input type="hidden" name="channel" value="{esc(ch)}">
  <label for="name-{esc(ch)}">Title</label>
  <input id="name-{esc(ch)}" name="name" value="{esc(title-or-empty)}"
         placeholder="Friendly name, e.g. Wire Guard" required>
  <button type="submit" class="btn btn-secondary">Save name</button>
</form>
<span class="help">Your label for this channel — how it reads in lists and pickers. The dialed address is changed under Address.</span>
```

- Prefill contract unchanged (stored title, else empty + plain placeholder —
  MANAGEMENT_DISCLOSURE_SPEC §5); ids keep the `name-{ch}` scheme so the
  pinned markup (`name="name" value="Tech Tips"`, `tests/channels.rs:275`;
  placeholder pin `:277-279`) survives verbatim.
- Button noun: **`Save name`** (section context makes "Edit" redundant; the
  PRG banner `Channel name saved.` already speaks this noun). The old
  `aria-label="Edit name of {ch}"` is dropped — the visible label + button
  text carry the name (nothing pins it). The input `name="name"` and
  `required` are unchanged so the handler and the no-clear honesty
  (`set_channel_title(None)` no-op) are untouched.

### 5.4 Address (owner)

Read-only display + the Change-address disclosure, byte-verbatim from
`channel_row_name_controls` (`channels_rows.rs:107-129`): hidden `channel`
field, `addr-{ch}` input prefilled with the current identifier, shape-
branched warning (`@handle changes on Telegram's side are not tracked here —
update it here too.` / `Private channels must keep their -100 ID. Changing
this retargets where posts go.`), the confirm (`Change where posts for {ch}
go? This retargets every plan and post that uses it.`), danger `Retarget`
submit to `/channels/rename`. Above it:

```html
<p>Posts dial <span class="mono">{esc(ch)}</span>.</p>
<details><summary>Change address…</summary>…verbatim form…</details>
```

### 5.5 Default (owner)

- Non-default row: the Set-default form byte-verbatim from
  `channels_rows.rs:43-52` (`POST /channels/default`, hidden `channel`,
  `btn btn-ghost btn-sm`, `aria-label="Set {ch} as default"`), plus help:
  `The default channel is the fallback posting target for plans and posts.`
- Default row: `<p>This is the default channel.</p><span class="help">Set
  another channel as default to remove this one.</span>` — the row hint
  (`channels_rows.rs:77`) re-homed and completed.

### 5.6 Posting as (owner)

The bind forms byte-verbatim from `channels_rows.rs:19-42`: user-bound row →
`Post as bot` (btn-secondary, no confirm); bot-bound row → `Post as user…`
(btn-ghost + the confirm naming the channel and the risk). The gate chain
(Enable-posting → Ready identity) and the audit writes are the handler's —
unchanged. The `as user` badge also rides the header badge row (state at a
glance), keeping the exception-only contract.

### 5.7 Danger zone (owner) / admin read-only

```html
<article class="panel danger-zone">
  <h2>Danger zone</h2>
  <p>Removing disconnects every plan and post that dials this address. This cannot be undone.</p>
  <!-- non-default rows: -->
  <form method="post" action="{url}/channels/remove" class="inline-form">
    <input type="hidden" name="channel" value="{esc(ch)}">
    <button type="submit" class="btn btn-danger" aria-label="Delete {esc(ch)}"
            onclick="return confirm('{confirm}')">Delete</button>
  </form>
  <!-- default rows: no button, the protection fact instead -->
  <span class="help">The default channel cannot be removed — set another channel as default first.</span>
</article>
```

- Confirm verbatim: `Remove {ch}? This cannot be undone.` (`js_arg` + `esc`,
  `channels_rows.rs:60-71`); `aria-label="Delete {ch}"` verbatim.
- The default-protection line reuses the handler's own refusal copy
  (`channels.rs:493-495`) so the page explains before the POST refuses.
- **Admin:** Identity/Address/Default/Posting render as read-only facts
  (label, mono address, default state, sender state); no forms, no Danger
  zone (admin is refused by every action route — `channels.rs:456,509,644,
  707`).

---

## 6. Flash routing — every key and where it now surfaces

`ChannelsQuery` is unchanged; `channels_banners`/`channels_error_copy` are
unchanged and are called by BOTH the list and the detail page (flag-keyed,
page-agnostic — old `/channels?flag=1` bookmarks keep rendering their
banners). The detail body simply never renders the added-callout.

| Key / path | Produced by | Surfaces on | Redirect after |
|---|---|---|---|
| `added=1` (legacy, no `channel`) | add without visibility | LIST (top banner) | unchanged |
| `added=1&channel=X[&notitle=1]` | visibility-aware add | LIST (callout + `row-added` reveal; top banner suppressed) | unchanged |
| `error=empty`, `error=private-id`, `error=public-shape` | add path | **ADD page** | `/channels/add?error=…` (was `/channels?error=…`) |
| add DB Err (duplicate) | add path | **ADD page** inline, focused | `channels_add_error` (was list `channels_error`) |
| `default=1` | POST /channels/default | DETAIL | `/channels/manage?channel=X&default=1` |
| `named=1` | POST /channels/name | DETAIL | `/channels/manage?channel=X&named=1` |
| `renamed=1[&same=1]` | POST /channels/rename (+ NOCASE guard) | DETAIL | `/channels/manage?channel=X&renamed=1[&same=1]` |
| `error={db}` from rename (NOCASE collision) | POST /channels/rename | DETAIL | `/channels/manage?channel={old}&error={e}` |
| `bound-user=1` / `bound-bot=1` | POST /channels/bind-sender | DETAIL | `/channels/manage?channel=X&bound-*=1` |
| `error=posting` / `error=locked` / `error=identity` | bind-sender refusals | DETAIL | `/channels/manage?channel=X&error=…` |
| `removed=1` | POST /channels/remove (owner AND seller) | LIST — the target row no longer exists | byte-identical |
| `error=empty` on default/remove/name/rename/bind-sender | scripted only (UI always carries `channel`) | LIST | byte-identical |
| inline 200 errors (`channels_error`) | scripted-only paths: default-removal refusal (Delete is hidden for default rows now), bind-sender shape/global refusals, name missing-row | LIST | byte-identical |

All `channel=` values ride redirects `urlencode_query`-ed (house rule,
`channels.rs:417,613,673`).

---

## 7. Seller view — same shape, honesty-gated sections

**Verified gating (citations, correcting the brief):**

| Action | Seller today | Evidence |
|---|---|---|
| Add / replace own channel | YES (add = `set_seller_channel`) | `channels.rs:423-430` |
| Delete own channel | **YES** — Delete is NOT owner-only | `channels.rs:501-508`; today's seller row already carries Delete (`channels_rows.rs:343-345`); cross-scope remove refused honestly (`tests/channels.rs:103-116`) |
| Rename (Change address) own channel | API-allowed (`rename_channel` takes the seller sid), **NO UI today** — adding one is a new capability, OUT OF SCOPE this wave (documented, not silently dropped) | `channels.rs:564-572` |
| Edit name (`POST /channels/name`) | NO — owner-only 403 | `channels.rs:644-646`; pin `tests/channels.rs:589` |
| Set default | NO — owner-only 403 | `channels.rs:456-458`; pin `tests/channels.rs:125` |
| Posting-as binding | NO — owner-only 403 | `channels.rs:707-709`; pin `identity_tests.rs:969` |

**Prescription:** sellers follow the same master-detail shape (consistency;
the builder is shared, so it is nearly free):

- **Seller master** (`GET /channels`): page-head + `Add channel` link;
  panel `Your channel`; their single row as a read-first row — label +
  type badge + `bot-managed` badge (+ notitle hint `no title stored for
  this channel yet`, verbatim `channels_rows.rs:328`) + `Manage →`
  (`aria-label` per §4.2); empty state per §4.3. The add/replace form moves
  to `GET /channels/add` (seller variant, §3.2) — for a seller add and
  replace are the same act, so it is the same page. The reveal
  (`channels_rows.rs:338-341`) is preserved.
- **Seller detail** (`GET /channels/manage?channel=X`, own channel only):
  header (label + `bot-managed` badge); **Identity** read-only (label fact
  + `no title stored for this channel yet` when true — a seller-typed title
  would be silently dropped, §3.2 of the add spec); **Address** read-only
  (mono display); **Default / Posting as: hidden** (owner-only — silence is
  honest, these were never the seller's controls); **Danger zone: Delete**
  (verbatim form/confirm/aria from `channels_rows.rs:333-345`; the
  consequence line from §5.7). Cross-scope X → §5.2 refusal.
- After seller Delete → `/channels?removed=1` (list; target gone) — the
  seller list re-renders with the empty state.

---

## 8. CSS — exactly one new rule

Everything reuses house classes: `.page-head`/`.actions` (`app.css:200-203`),
`.panel`, `h2`, `.mono`, `.badge*`, `.alert*`, `.help`, `.help-more`,
`.btn`/`.btn-secondary`/`.btn-ghost`/`.btn-danger`/`.btn-sm`,
`.row-actions`, `.inline-form`, `.row-added`. The Manage affordance needs no
new style (`.btn` already flexes its `gap: 7px` around the arrow,
`app.css:388-392`). The list rows already flex (`app.css:420-440`).

```css
/* Channels manage view: the danger zone panel leads with the danger
   hairline — same left-border idiom as .alert (app.css:504). */
.danger-zone { border-left: 3px solid var(--danger); }
```

Token-based, no new colors; AA in both themes (danger border on surface —
the same pairing `.alert-error` ships, `app.css:511`).

---

## 9. File plan and wc projections

| File | Today | Moves/edits | Projected |
|---|---|---|---|
| `src/web/channels.rs` | 783 | − add-form block (`:240-268`, 29) − page-head rewrap of `render_global_channels` (+3) − redirect-string edits across six handlers (±0) − add-Err branch swap (+1) − doc/comment updates (+4) | **≈ 762** (target ≤800) |
| `src/web/channels_rows.rs` | 387 | − `channel_row_actions_owner` (`:8-82`, 75) − `channel_row_name_controls` (`:84-130`, 47) — both MOVE into `channels_manage.rs` as section builders − row builder rewritten to badges+Manage (−8) − seller form moves to add page (−17), seller row gains Manage (+5) − callout/hint copy re-points (±0) | **≈ 250** |
| `src/web/channels_manage.rs` | — | NEW: `channels_add_page` GET handler (~26), add-page builder owner/seller (~75), `channels_add_error` (~10), `channels_manage_page` GET handler + resolution/refusals (~60), detail body + section builders moved from the row family (~190) | **≈ 370** |
| `src/web/mod.rs` | — | `mod`/`use` (+2); `:246` route line gains `get(channels_add_page)`; `routes_channels` gains `/channels/manage` (+1) | +3 |
| `assets/app.css` | 672 | +1 rule (§8) | 673 |

The detail builders live in their own module, not `channels_rows.rs`:
the row file stays the row-rendering family (its §2.3 charter), and
`channels.rs` keeps every POST handler so the mutation surface remains
greppable in one place. No file crosses the 800 target; no function exceeds
100 lines.

---

## 10. Test risk — the complete at-risk inventory

**Day's biggest churn, stated honestly: 36 assertions across 2 files —
28 URL-moves (copy survives verbatim, the test GETs a different page) and
8 Location rewrites (redirect targets change). Zero pinned copy changes;
zero smoke pins break.**

### 10.1 Assertion URL-moves — copy verbatim, test URL changes (28)

| Test / location | Pinned string (verbatim) | New home |
|---|---|---|
| `tests/channels.rs:182` | `@handle or a t.me link — the bot must be admin in the channel.` | `GET /channels/add` (owner) |
| `tests/channels.rs:186` | `<details class="help-more"><summary>Which forms are accepted?</summary>` | `GET /channels/add` |
| `tests/channels.rs:190` | `Adding one does not change the default — use Set default.` | `GET /channels/add` |
| `tests/channels.rs:195` | `<fieldset class="visibility">` | `GET /channels/add` |
| `tests/channels.rs:196` | `data-visibility-form` | `GET /channels/add` |
| `tests/channels.rs:198` | `data-ph-public="@handle" data-ph-private="-1001234567890123"` | `GET /channels/add` |
| `tests/channels.rs:202` | `<input id="ch-title" name="title"` | `GET /channels/add` |
| `tests/channels.rs:211` | seller help one-liner (same string) | `GET /channels/add` (seller) |
| `tests/channels.rs:214` | `Adding one replaces the previous.` | `GET /channels/add` (seller) |
| `tests/channels.rs:215` | `<fieldset class="visibility">` (seller) | `GET /channels/add` (seller) |
| `tests/channels.rs:217` | NOT-contains `<input id="ch-title" name="title"` (seller) | `GET /channels/add` (seller) |
| `tests/channels.rs:267-270` | `Nothing changed — that's still the channel's address.` | `GET /channels/manage?channel=@one&renamed=1&same=1` |
| `tests/channels.rs:271` | NOT-contains `Channel renamed to` | same detail URL |
| `tests/channels.rs:275` | `name="name" value="Tech Tips"` | `GET /channels/manage?channel=@one` |
| `tests/channels.rs:277-279` | `name="name" value="" placeholder="Friendly name, e.g. Wire Guard"` | `GET /channels/manage?channel=@two` |
| `tests/channels.rs:412-416` | `Private channels are addressed by their numeric -100 ID only — …` | `GET /channels/add?error=private-id` |
| `tests/channels.rs:648` | `Change address…` | `GET /channels/manage?channel=-1001234567890123` |
| `tests/channels.rs:649-654` | `Private channels must keep their -100 ID. Changing this retargets where posts go.` | same detail URL |
| `tests/channels.rs:655` | `>Retarget</button>` | same detail URL |
| `tests/channels.rs:656` | `action="/channels/rename"` | same detail URL |
| `tests/channels.rs:677-681` | `Nothing changed — that's still the channel's address.` | `GET /channels/manage?channel=-1001234567890123&renamed=1&same=1` |
| `tests/channels.rs:618` | `Channel name saved.` | `GET /channels/manage?channel=@one&named=1` |
| `tests/channels.rs:619` | `Tech Tips (@one)` | `GET /channels/manage?channel=@one` |
| `identity_tests.rs:1146` | `Post as bot` | `GET /channels/manage?channel=@userbound` |
| `identity_tests.rs:1148` | `aria-label="Post to @userbound as the bot"` | same detail URL |
| `identity_tests.rs:1154` | `Post as user…` | `GET /channels/manage?channel=@botbound` |
| `identity_tests.rs:1156` | `aria-label="Post to @botbound as your personal account"` | same detail URL |
| `identity_tests.rs:1159-1164` | `Bind @botbound to your personal account? Posts to this channel will be sent as you…` | same detail URL |

### 10.2 Location rewrites — redirect target changed (8)

| Test / location | Old pin | New pin |
|---|---|---|
| `tests/channels.rs:248-250` (NOCASE guard) | `/channels?renamed=1&same=1` | `/channels/manage?channel=@one&renamed=1&same=1` |
| `tests/channels.rs:288-293` (real rename) | `/channels?renamed=1&channel=TechTips` | `/channels/manage?channel=TechTips&renamed=1` |
| `tests/channels.rs:667-672` (unchanged-submit guard) | `/channels?renamed=1&same=1` | `/channels/manage?channel=-1001234567890123&renamed=1&same=1` |
| `tests/channels.rs:600-605` (Edit name) | `/channels?named=1&channel=@one` | `/channels/manage?channel=@one&named=1` |
| `identity_tests.rs:990-995` (bind bot) | `/channels?bound-bot=1&channel=@chan` | `/channels/manage?channel=@chan&bound-bot=1` |
| `identity_tests.rs:1019-1024` (gate refusal) | `/channels?error=posting` | `/channels/manage?channel=@chan&error=posting` |
| `identity_tests.rs:1041-1045` (readiness refusal) | `/channels?error=identity` | `/channels/manage?channel=@chan&error=identity` |
| `identity_tests.rs:1057-1062` (bind bot again) | `/channels?bound-bot=1&channel=@chan` | `/channels/manage?channel=@chan&bound-bot=1` |

### 10.3 Explicitly surviving (verified, zero edits)

- `smoke_tests.rs:168-175` `/channels` authed 200; `:177-183` unauth →
  `/login`; `:268-278` POST `/channels/add` redirect; `:280-288` unauth
  redirect; `:508-509` `/channels/title|desc` authz probes. **All 200-pins
  keep passing** — the master list still renders 200 for owner/seller/admin.
- `tests/channels.rs` scoping test: `:49-50` (`@one`/`@two`/`default`),
  `:58-60` (`cannot be removed` inline 200), `:89,97-98,100-102`
  (`@mine`, `bot-managed`, NOT `Global channels`), `:112` (`not yours to
  remove`), `:125` (seller default 403), `:149,156,159` (scoped-empty
  copies).
- `tests/channels.rs:377-378,385-392,458-477,489-513` — add redirect
  contains-pins (`added=1`, `channel=@wireguard`, `notitle=1`) and master
  labels (`>public<`, `Wire Guard (@foo)`) — the add PRG target is
  unchanged.
- `tests/channels.rs:521-566` — the whole reveal/suppression test,
  byte-identical behavior.
- `tests/channels.rs:298-350` — dashboard composer dropdown (untouched).
- `identity_tests.rs:1143` — exactly one `>as user</span>` on the master
  list (badges are state, kept on rows).
- `identity_tests.rs:1096,1109,1127` — bind-sender inline refusal copy on
  200 (list-shaped `channels_error` unchanged).
- `tests/misc.rs`, `tests/bots.rs`, `tests/settings.rs`, `tests/setup.rs`,
  `tests/subs.rs`, `timeout_tests.rs`, `security_tests.rs` — no channel
  page pins.
- Base-path pins: none exist for channels (`base_path: ""` in
  `test_support.rs:46`, `tests/mod.rs:54`, `tests/setup.rs:57`); all new
  hrefs/actions go through `state.url()` so a future base path stays safe.

### 10.4 Additive tests to prescribe

1. **Add page smoke + roles:** GET `/channels/add` → owner 200 (contains
   `ch-title`, `fieldset.visibility`, `Add channel` submit, back link),
   seller 200 (contains `Set channel`, NOT `ch-title`), unauth → `/login`,
   admin → 403.
2. **Master list is read-first:** owner GET `/channels` contains
   `>Manage` + `aria-label="Manage Wire Guard"` and NOT `Edit name`,
   NOT `Change address…`, NOT `<form` inside rows beyond zero inline-form
   count in the list `<ul>`; contains `Add channel` link to
   `/channels/add`.
3. **Manage happy path:** GET `/channels/manage?channel=@one` → 200 with
   `Back to channels`, the label, `action="/channels/name"`,
   `Set default`, `Post as user…`, `action="/channels/remove"`.
4. **Manage refusals:** unknown X → `That channel isn't in the global list`;
   bot-scoped owner + foreign-bot row → `belongs to another bot`; seller +
   foreign X → `isn't yours to manage`; seller + own X → facts + Delete,
   NOT `Set default`/`Post as`.
5. **Detail flash surfacing:** GET
   `/channels/manage?channel=@one&default=1&named=1` renders both banners
   (matrix §6); legacy `/channels?renamed=1&same=1` still renders the guard
   copy (legacy tolerance pin).
6. **Smoke additions:** `smoke_channels_add_page_authed_200`,
   `smoke_channels_add_page_unauth_redirect`,
   `smoke_channels_manage_authed_200`, `smoke_channels_manage_unauth_redirect`.

---

## 11. A11y checklist (binding)

- [ ] Manage affordance has a per-channel accessible name:
      `aria-label="Manage {title-or-identifier}"`; the `→` is
      `aria-hidden`.
- [ ] Detail heading order: one `h1` (the channel label), sections `h2`,
      no skipped levels; the rail landmark is unchanged.
- [ ] Add page: one `h1`; the form moves with its labels, `aria-describedby`
      swap ids, and `help-more` intact (JS-off path works: public help
      visible by default).
- [ ] Focus on load preserved: the `row-added` row keeps `tabindex="-1"`
      + `data-focus-on-load` on the LIST; the identity unlock banner still
      outranks it (`html.rs:46-53`). PRG error banners on the add page use
      `role="alert"` (assertive); success banners `role="status"` (polite) —
      the `channels_banner` contract (`channels.rs:30-37`) everywhere.
- [ ] Back to channels is a real link with a name; danger controls keep
      their consequence-named confirms; every hidden input carries the RAW
      identifier (escaped), visible prose may use the label.
- [ ] Visible focus rings everywhere (house `:focus-visible`, `app.css:154`);
      contrast pairs are token pairs (DESIGN.md §4).

---

## 12. Acceptance checklist

- [ ] `GET /channels/add` (owner/seller variants) + `GET /channels/manage`
      live; `POST /channels/add` keeps its handler and its success PRG;
      zero new POST routes; `mod.rs` wiring per §2.
- [ ] Master rows: label + state badges + ONE `Manage →` affordance; no
      inline forms in rows; the default hint, Edit name, Change address,
      Delete all live on the detail page.
- [ ] Detail sections in order Identity → Address → Default → Posting as →
      Danger zone; gates verified (default/name/bind owner-only; delete
      owner-global OR seller-own; admin read-only).
- [ ] Flash matrix §6 exact: added stays list-level with the reveal
      byte-identical; `removed` stays list-level; the other flags surface
      on the detail page; legacy `/channels?flag=1` URLs still render
      banners (legacy tolerance).
- [ ] Scoped manage refusals match the scoped-empty register; seller
      cross-scope fails closed.
- [ ] `channels.rs` ≈762, `channels_rows.rs` ≈250, new
      `channels_manage.rs` ≈370 — none over target; `cargo build && cargo
      test && cargo clippy -- -D warnings && cargo fmt --check` pass.
- [ ] `app.css` grows by exactly one rule; visual check light AND dark at
      375px (row wraps: label/badges then Manage; detail panels stack).
- [ ] The 36-assertion inventory of §10 is applied; smoke 200-pins pass
      untouched; additive tests of §10.4 land in
      `src/web/tests/channels.rs` (+ smoke additions in
      `src/web/smoke_tests.rs`).
