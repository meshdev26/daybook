# Channel Add Redesign — Visibility-Aware Add, Post-Add Reveal, Name vs Address Split

**Status:** Approved, ready for web implementation
**Date:** 2026-11-01
**Discipline:** design
**Territory of this spec:** the Channels page add form, add flow, row
actions, and post-add reveal. Code changes land in `src/web/channels.rs`, a
new `src/web/channels_rows.rs`, `src/web/html.rs` (APP_JS block), a small
additive block in `assets/app.css`, `src/web/mod.rs` (one route line + module
wiring), and `src/web/tests/channels.rs`. Nothing else.
**Sources of truth:** `DESIGN.md` + `assets/app.css` (MASTER.md is deprecated
2026-10-31 and must not be used). Related specs this one builds on and must
not disturb: `CHANNELS_TITLE_UX_SPEC.md` (label contract, honest add),
`MANAGEMENT_DISCLOSURE_SPEC.md` §4–§5 (collapsed help, honest banners),
`IDENTITY_SENDER_UX_SPEC.md` §2 (badge placement).
**Hard constraints:** `channels.rs` is AT its 800-line cap — step 0 is a
move-only split (file & module hygiene rule: "At cap: no further
growth — the next change splits first"). No DB changes (both Db methods this
spec needs already exist). Exactly one new route. `esc()` everything dynamic.

---

## 0. Ground truth (owner-verified 2026-11-01 — encode as the model)

1. **`channels.channel` is the TELEGRAM IDENTIFIER the bot dials.** Public
   channel → `@handle`. Private channel → numeric `-100…` ID ONLY. There is
   no third form.
2. **Rewriting `channels.channel` retargets the channel.**
   `Db::rename_channel` (`src/db.rs:5650-5691`,
   `UPDATE channels SET channel = ?1 …`) changes where every plan, post,
   subscription, and scheduled send points. Giving it a display name — or a
   private channel's handle — is not a rename, it is a retarget.
3. **`channels.title` is the local display label** — human-facing, never
   dialed. Today it is written in exactly one place: the owner branch of the
   add flow (`src/web/channels.rs:511` → `Db::set_channel_title`,
   `src/db.rs:2031-2047`, best-effort NOCASE update on the `channels` row).
   No UI edits it after add. `set_channel_title` exists and is reusable —
   this spec reuses it; no new Db method.
4. **Telegram facts the UI must respect:**
   - A public channel has a `username`; `getChat` reveals it. A channel
     without a `username` is private.
   - A private channel is addressable only by its `-100…` numeric ID.
   - Posting requires the bot to be an **admin** in the channel.
   - Telegram **never notifies bots** about username/title changes, so the
     stored identifier and title both go stale silently. A user-visible
     refresh/remedy action is the only cure (see §9).

---

## 1. Verified current state (file:line)

| Fact | Location |
|---|---|
| Owner add form: single `channel` box, omnibus help | `src/web/channels.rs:251-261` (form), help one-liner + `help-more` details at `:254` |
| Seller add form: same single box, same omnibus help | `src/web/channels.rs:207-218` (help at `:211`) |
| Add handler, best-effort resolution | `src/web/channels.rs:458-519`; `channel_chat_info` call at `:486`; redirect flags at `:493-497`; owner `set_channel_title` at `:511` |
| `channel_chat_info` resolves @handle/t.me → **numeric id** when the bot answers | `src/web/content.rs:215-266` (handle branch `:242-266`) |
| Unused normalizer: t.me → `@handle`, `@` → `@` (has `#[allow(dead_code)]`) | `src/web/content.rs:183-208` (`normalize_channel_input`) |
| Rename form struct — 14 alias fields through `first_non_empty` | `src/web/channels.rs:589-627` |
| Rename handler + NOCASE self-rename guard | `src/web/channels.rs:630-694` (guard `:679-681`, `rename_channel` call `:684`) |
| Row builder + inline Rename form | `src/web/channels.rs:275-402` (Rename form `:362-375`) |
| Badge keys off identifier shape | `src/web/channels.rs:416-422` (`channel_type_badge`: `starts_with('@')` → `public`, else `private`) |
| Shared label helpers | `channel_label` `src/web/channels.rs:428-433`; `channel_titles` `:439-450` |
| `data-focus-on-load` APP_JS pattern (focuses FIRST match; unlock banner wins) | `src/web/html.rs:31-37` (selector `:36`) |
| Routes: `/channels`, `/channels/add`, plus `routes_channels` | `src/web/mod.rs:243-244`, `:251-255` |
| `POST /channels/title` is ALREADY TAKEN — Bot API `set_chat_title` | `src/web/mod.rs:359` → `src/web/channel_ops.rs:264-281` |
| `Db::set_channel_title` (no Result; no-op on missing row / empty title) | `src/db.rs:2031-2047` |
| `Db::rename_channel` (identifier rewrite, scope + UNIQUE NOCASE) | `src/db.rs:5650-5691` |
| `list_channels` — `ORDER BY id` (new rows render last) | `src/db.rs:5778-5786` |
| `to_chat_id` dials `@handle` fine (`Recipient::ChannelUsername`) | `src/util.rs:4-13` (test `:319`) |
| Row-existence probe used by bind-sender | `state.db.get_channel_seller_id(ch) != Some(0)` — `src/web/channels.rs:743` |
| House CSS: `--brand-tint` ("selected rows, active nav") | `assets/app.css:50` |
| House CSS: checkbox width-auto block (radio needs the mirror) | `assets/app.css:287-314`; `label.check` `:295-298` |
| House CSS: `label` / `input` / focus ring / `.help` / `.help-more` / `.form-row` | `assets/app.css:313-317`, `:316-327`, `:329-332`, `:336`, `:339-341`, `:344` |
| House CSS: row list flex, `.row-actions`, `.mono`, `.badge*` | `assets/app.css:406-427` (`.row-actions` `:422`), `:433`, `:469-487` |
| House CSS: `.alert*`, `details` in rows (precedent for row-level disclosure) | `assets/app.css:491-505`, `:519-533` (`li details` margin fix `:533`) |
| `.sr-only` exists | `assets/app.css:594`; used in the current Rename form `src/web/channels.rs:359` |
| Cap rule + move-only split rule | file & module hygiene rules |

Test files scanned: `src/web/tests/channels.rs` (329 lines) and
`src/web/tests/misc.rs` (219 lines). **`misc.rs` contains no channel add or
rename pins at all** — it is the `esc()` proptest (`misc.rs:13-45`), auth
flows, and dashboard/privacy checks; nothing there is at risk, and its
`esc()` proptest actively guards this spec's markup.

---

## 2. Step 0 — the split (binding; do this FIRST, separately reviewed)

`src/web/channels.rs` is exactly **800 lines** — at cap. Per
the file & module hygiene rule, the next change splits first. The boundary is the
row/label rendering family:

### 2.1 Move list (exact functions, exact line counts)

| Function | Current lines | Count |
|---|---|---|
| `render_global_channel_row` | `channels.rs:275-402` | 128 |
| `channel_type_badge` | `channels.rs:416-422` | 7 |
| `channel_label` | `channels.rs:424-433` | 10 |
| `channel_titles` | `channels.rs:435-450` | 16 |
| **Total moved** | | **161** |

(The task's minimum — `render_global_channel_row` + `channel_type_badge` — is
135 lines. Move all four: they are one family — everything that renders a
channel row, label, or badge — and the extra 26 lines of headroom are needed
by §3–§5. Still move-only.)

New file `src/web/channels_rows.rs`, first line `use super::*;` (house
pattern, `channels.rs:1`). Visibility: `render_global_channel_row` and
`channel_type_badge` become `pub(super)` (they gain cross-module callers);
`channel_label` / `channel_titles` keep their existing `pub(super)`. Wire in
`src/web/mod.rs` next to `mod channels;` (`mod.rs:35`) /
`use channels::*;` (`mod.rs:79`):

```rust
mod channels_rows;
use channels_rows::*;
```

Callers in `dashboard.rs`, `content.rs`, `subs.rs`, `admin_console.rs`,
`monitor.rs` keep compiling unchanged — they read `channel_label` /
`channel_titles` through the `use super::*;` glob chain.

### 2.2 Move-only discipline + one sanctioned mechanical follow-up

- The move itself changes **no bytes** of any function body
  (file & module hygiene rule 2: reviewed as a rename). The existing test suite pins
  row markup, so regressions are caught (`tests/channels.rs:49-50, 98, 149`).
- `render_global_channel_row` is **128 lines** — already over the 100-line
  function budget (flagged as pre-existing by CHANNELS_TITLE_UX_SPEC §5.6).
  Because §5 edits it anyway, the same PR may — immediately after the move,
  before any redesign edit — decompose it mechanically into three private
  helpers inside `channels_rows.rs`, each ≤ 100 lines, with **byte-identical
  output** (the pinned assertions are the oracle):
  - `channel_row_actions_owner(state, is_owner, row)` — bind-sender /
    Set default / Delete / default hint forms (`channels.rs:296-350`);
  - `channel_row_name_controls(state, is_owner, row)` — Edit name form +
    Change address disclosure (§5 replaces the old Rename form
    `channels.rs:352-375`);
  - `render_global_channel_row` — label cluster + `<li>` assembly only.

### 2.3 Budget after the split

| File | After step 0 | Redesign adds (§3–§5) | Result |
|---|---|---|---|
| `src/web/channels.rs` | 800 − 161 = 639 | ≈ +115 (visibility validation ≈ +28, redirect param ≈ +3, banners: `named` flag + suppression + two fixed error keys ≈ +16, `ChannelsQuery.named` ≈ +2, `channels_name` handler + form ≈ +40, seller radio ≈ +12, callout/highlight plumbing ≈ +8, doc comments ≈ +6) | ≈ **754** — under target |
| `src/web/channels_rows.rs` | 161 | ≈ +40 (row `row-added` attrs +6, Edit name form net +2, Change address details +16, callout helper +16) | ≈ **201** |

Overflow valve (use before trimming any prescribed copy): move
`render_seller_channels` (`channels.rs:165-219`, 55 lines) into
`channels_rows.rs` as well — it is row rendering by the same definition.

---

## 3. Item 1 — Kill the `@yourchannel` box: the unified visibility-aware add form

### 3.1 Owner/global form (replaces `channels.rs:251-261`)

```html
<form method="post" action="{base}/channels/add" data-visibility-form>
<label for="ch-title">Title</label>
<input id="ch-title" name="title" maxlength="64" autocomplete="off" placeholder="e.g. Wire Guard">
<span class="help">Your label for this channel — how it reads in lists and pickers. Leave empty and I'll fill it from Telegram when the bot can see the channel.</span>

<fieldset class="visibility">
<legend>Visibility</legend>
<label class="check"><input type="radio" name="visibility" value="public" checked> Public — it has an @handle anyone can find</label>
<label class="check"><input type="radio" name="visibility" value="private"> Private — invite-only; addressed by its numeric ID</label>
</fieldset>

<label for="ch-id">Channel</label>
<input id="ch-id" name="channel" required autocomplete="off" spellcheck="false"
       data-ph-public="@handle" data-ph-private="-1001234567890123" placeholder="@handle"
       aria-describedby="ch-id-help-public">
<span class="help" id="ch-id-help-public">@handle or a t.me link — the bot must be admin in the channel.</span>
<span class="help" id="ch-id-help-private" hidden>Its numeric -100 ID only — forward any message from the channel to @userinfobot to get the ID.</span>

<details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Public: @handle or a t.me link (stored as @handle). Private: the numeric chat ID only — it starts with -100; forward any message from the channel to @userinfobot to get it. The bot must be an admin in the channel. Adding one does not change the default — use Set default.</span></details>

<button type="submit">Add channel</button>
</form>
```

Rules encoded here:

- **Title first, always user-set.** A typed title is the human label and
  WINS over getChat (§9). Empty title + resolvable getChat → auto-fill
  (today's behavior, `channels.rs:486-511`, preserved).
- **Progressive disclosure per the house idiom** (MANAGEMENT_DISCLOSURE_SPEC
  §4): one visible help line per state; the old omnibus help collapses into
  the existing `help-more` details — same `summary` string
  (`Which forms are accepted?`) so the pinned assertion at
  `tests/channels.rs:183` survives; the owner tail sentence
  `Adding one does not change the default — use Set default.`
  (`tests/channels.rs:187`) is kept verbatim inside the details.
- **The visibility radio is the truth, the server is the enforcement** —
  the radio adapts the form live (JS below), but validation is server-side
  (§3.4). Without JS: public help shows by default, private help is
  `hidden`, and the omnibus details still carries both rules — the page
  works with JS off (html.rs house comment, `html.rs:31-33`).

### 3.2 Seller form (replaces the help block at `channels.rs:211`)

Same `fieldset.visibility` radio + identifier input + adaptive help +
`help-more` details, with the seller tail kept verbatim:
`Adding one replaces the previous.` (`tests/channels.rs:197`). **No Title
field for sellers this wave** — seller channels live in
`sellers.default_channel` (`set_seller_channel`, `channels.rs:503`), and
`set_channel_title` only matches `channels` rows, so a seller-typed title
would be silently dropped. A control that does nothing violates the honesty
bar (same reasoning as CHANNELS_TITLE_UX_SPEC §2.4). Seller title capture
stays a follow-up (§9.3).

### 3.3 Adaptive JS contract (add to APP_JS first IIFE `init()`, `html.rs:14-38`, after the secret-toggle block; ~18 lines, same idempotent-bind idiom as `html.rs:18-19`)

```js
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
```

Exact DOM contract: form `[data-visibility-form]`; radios
`input[name="visibility"]` values `public|private`; input
`input[name="channel"]` with `data-ph-public` / `data-ph-private`; help spans
`#ch-id-help-public` / `#ch-id-help-private`. Vanilla, no new dependencies,
`hidden` attribute toggling only.

### 3.4 Server-side validation in `channels_add` (`channels.rs:458-519`)

`ChannelForm` (`channels.rs:452-455`) gains `visibility: Option<String>` and
`title: Option<String>`. Validation order after the existing empty-check
(`:467-470`):

1. **`visibility` missing → legacy omnibus path** (today's behavior verbatim,
   no shape enforcement). The console forms always send it; scripted and
   legacy posters keep working. This is a UX gate, not a security boundary —
   `getChat` resolution stays best-effort — so fail-open here is correct and
   keeps `smoke_channels_add_authed` (`smoke_tests.rs:262-272`) and
   `tests/channels.rs:24-32` green untouched.
2. **`visibility == "private"`**: the trimmed input must parse as `i64` AND
   start with `-100`. Otherwise redirect
   `/channels?error=private-id`.
3. **`visibility == "public"`**: run `normalize_channel_input`
   (`content.rs:183-208` — currently dead code; reusing it removes the
   `#[allow(dead_code)]`). It yields `@handle` from `@handle`/`t.me/<handle>`.
   A numeric input under Public → redirect `/channels?error=public-shape`
   (the user pasted a private ID; the copy points at the switch).
   `None` (bare title or junk) → redirect `/channels?error=public-shape`
   too — one honest refusal beats storing garbage.
4. **Storage follows the corrected model (§0.1): public rows are stored as
   `@handle`.** `channel_chat_info` (`content.rs:215-266`) still runs for the
   TITLE, but its numeric id is discarded on the public path — today a
   resolved public channel is stored as `-100…`, which both contradicts the
   model and makes the shape-derived badge lie (§6). Dialing `@handle` is
   fully supported (`util.rs:4-13`). Private path keeps the numeric id.
5. **Title choice:** user-typed title (`Some`, non-empty after trim) >
   getChat title > none. `notitle=1` rides the redirect only when BOTH are
   absent (a typed title means nothing is unresolved).
6. **Redirect carries the new anchor (§4):**
   `format!("/channels?added=1&channel={}", urlencode_query(&canonical))`,
   plus `&notitle=1` when applicable. (`urlencode_query`, `html.rs:108-119`;
   `state.url` still base-prefixes the formatted path.)

New fixed error keys in the `channels_banners` match (`channels.rs:91-120` —
fixed keys render fixed copy; unknown values stay escaped):

- `private-id` → `Private channels are addressed by their numeric -100 ID only — paste the ID, or switch Visibility to Public if the channel has an @handle.`
- `public-shape` → `Public channels are addressed by an @handle or a t.me link — or switch Visibility to Private and paste the channel's numeric -100 ID.`

---

## 4. Item 2 — Post-add reveal: highlighted row + focused callout

Mechanism (three parts, all server-rendered; APP_JS needs **no changes** for
this item — `html.rs:36-37` already focuses the first
`[data-focus-on-load]`):

1. **Anchor:** the add redirect carries `?added=1&channel=<canonical>`
   (§3.4.6). `ChannelsQuery.channel` already exists (`channels.rs:142`) — no
   new query field for the anchor.
2. **Highlighted, focused row:** in `render_global_channel_row`, when
   `added == Some("1")` and `q.channel` NOCASE-matches `row.channel`, the
   `<li>` renders:

   ```html
   <li class="row-added" tabindex="-1" data-focus-on-load>…badges + row-actions unchanged…</li>
   ```

   `tabindex="-1"` + `data-focus-on-load` is exactly the html.rs:36 pattern
   (same contract as identity/dashboard error alerts,
   `identity.rs:243`, `dashboard.rs:258`). APP_JS focuses it on load, which
   also scrolls it into view (`list_channels` is `ORDER BY id`
   (`db.rs:5782-5786`) — new rows render last, so the scroll lands on it).
   Same match in `render_seller_channels` for seller adds.

3. **The callout** — rendered inside the panel, immediately **after the
   `<ul>`** (adjacent to the highlighted last row, before the add form), by a
   new `pub(super) fn channel_added_callout(name: &str, notitle: bool, is_owner: bool) -> String`
   in `channels_rows.rs`:

   ```html
   <div class="alert alert-info" role="status">Wire Guard is in. The bot must be an admin in it before anything posts — add it in Telegram, then use Set default if this is your main channel.</div>
   ```

   - `{name}` = the stored **title** when one exists, else the identifier —
     plain prose register, `esc()`-ed. (The adjacent highlighted row carries
     the full `Title (id)` label per CHANNELS_TITLE_UX_SPEC §4; the callout
     is speech, not an identifier surface, and matches the owner's sample
     copy. Deliberate, documented deviation from `channel_label` here only.)
   - **notitle variant** (redirect also carried `&notitle=1`) — replaces the
     "couldn't fetch" sentence, does not stack it:
     `{name} is in. I couldn't fetch its name from Telegram. The bot must be an admin in it before anything posts — add it in Telegram, then use Set default if this is your main channel.` — owner gets the tail
     ` Use Edit name to give it a label.` (action renamed per §5).
   - **Role honesty:** the `then use Set default if this is your main
     channel.` clause renders **only for owners** (admins/sellers have no
     Set default button — same reasoning as CHANNELS_TITLE_UX_SPEC §2.4).
     Seller variant: `{name} is in. The bot must be an admin in it before anything posts — add it in Telegram.`
   - Rendered whenever `added=1` and a `channel` param is present — including
     when the row itself is filtered out by a bot-scope selection — so the
     outcome message never disappears.

4. **One outcome, one banner** (precedent: CHANNELS_TITLE_UX_SPEC §3.2 —
   "showing both would repeat 'Channel added' twice"). In
   `channels_banners` (`channels.rs:41-122`): when `added=1` **and**
   `q.channel` is non-empty, the `added` arm emits **no** top banner
   (neither the plain success nor the notitle info) — the in-panel callout
   owns the message. Legacy redirects without `channel` keep today's banners
   byte-identical (`Channel added.` / the notitle info line).

### Banner × callout matrix

| Redirect | Top banner | In-panel callout | Row |
|---|---|---|---|
| `?added=1` (legacy) | `Channel added.` (success) | none | none |
| `?added=1&notitle=1` (legacy) | notitle info (today's copy, tail now says Edit name) | none | none |
| `?added=1&channel=X` | none (suppressed) | success callout | `row-added`, focused |
| `?added=1&notitle=1&channel=X` | none (suppressed) | notitle callout | `row-added`, focused |

### A11y + focus notes (binding)

- APP_JS focuses the **first** `[data-focus-on-load]`, and the identity
  unlock banner branch returns first (`html.rs:34-35`). When the owner's
  identity is locked, the security banner keeps focus priority and the row
  is not focused — intentional: the gate outranks the highlight. The
  `row-added` tint still marks the row visually.
- Programmatic focus on `tabindex="-1"` shows no `:focus-visible` ring — the
  `row-added` background is the visible affordance; keyboard users still get
  the standard ring when tabbing (`app.css:116`).
- The callout uses `role="status"` to match every banner on this page
  (`channels_banner`, `channels.rs:30-37`); it renders on page load, so it is
  announced once and does not double-announce with the (suppressed) top
  banner.
- Focus order is DOM order: focus lands on the row (label + badges +
  actions announced), the callout follows immediately after the `<ul>`.

### Prescribed CSS additions (the ONLY app.css changes in this spec; token-based, no new colors)

```css
/* Visibility radio group (channels add form): mirror the checkbox block so
   the 100%-wide input rule does not balloon the radios. */
fieldset.visibility { border: 0; padding: 0; margin: 12px 0 0; }
fieldset.visibility legend { font-size: 12.5px; font-weight: 600; color: var(--ink-2); padding: 0; }
fieldset.visibility label { margin-top: 8px; }
input[type="radio"] { width: auto; margin: 0 6px 0 0; vertical-align: middle; }

/* Post-add reveal: the just-added row (token comment: "selected rows"). */
.panel ul li.row-added { background: var(--brand-tint); }
```

Contrast: `--brand-tint` background with default `--ink` text — the same pair
`.alert-success` ships (`app.css:503`); AA in both themes (DESIGN.md §4).

---

## 5. Item 3 — Split the two "rename" concepts forever

`rename_channel` rewrites the dialed identifier (§0.2). The current UI gives
one box for both jobs. Two controls, two nouns, two endpoints:

### 5a. "Edit name" — safe, touches `title` only

Replaces the Rename inline form (`channels.rs:352-375`) in
`channel_row_name_controls`. Owner-only (gate unchanged from
`is_owner`, `channels.rs:362`):

```html
<form method="post" action="{base}/channels/name" class="inline-form">
<input type="hidden" name="channel" value="{esc(&row.channel)}">
<label class="sr-only" for="name-{row.channel}">Edit name of {esc(&row.channel)}</label>
<input id="name-{row.channel}" name="name" value="{esc(title-or-empty)}" placeholder="Friendly name, e.g. Wire Guard" required>
<button type="submit" class="btn btn-secondary btn-sm" aria-label="Edit name of {esc(&row.channel)}">Edit name</button>
</form>
```

- Prefill contract unchanged from today's Rename (MANAGEMENT_DISCLOSURE_SPEC
  §5): stored title when one exists, else EMPTY with the plain placeholder
  (`channels.rs:356-361` logic, new field name). `required` keeps an empty
  submit from ever reaching the Db (where `set_channel_title(None)` is a
  no-op, `db.rs:2035-2037` — the console cannot clear a title this wave;
  noted in §9).
- No retarget risk is possible: the handler never touches
  `channels.channel`.
- The titleless row hint copy updates to point at the new noun:
  `no title yet — use Edit name to name it` (`channels.rs:388-392`); the
  notitle banner tail likewise (`channels.rs:49`). Neither string is pinned
  in tests (verified — no test contains "no title yet" or "Rename to name
  it").

### 5b. "Change address" — danger, collapsed, touches `channel` via the existing rename path

In `channel_row_name_controls`, after the Edit name form (owner-only):

```html
<details>
<summary>Change address…</summary>
<span class="help">{warning — shape-branched below}</span>
<form method="post" action="{base}/channels/rename" class="inline-form">
<input type="hidden" name="channel" value="{esc(&row.channel)}">
<label class="sr-only" for="addr-{row.channel}">New address for {esc(&row.channel)}</label>
<input id="addr-{row.channel}" name="new" value="{esc(&row.channel)}" required spellcheck="false">
<button type="submit" class="btn btn-danger btn-sm" aria-label="Change the address of {esc(&row.channel)}" onclick="return confirm('{confirm-msg}')">Retarget</button>
</form>
</details>
```

- **Warning copy, branched on the row's identifier shape at render** (same
  `starts_with('@')` predicate as the badge, `channels.rs:416-422`):
  - private row (verbatim from the owner): `Private channels must keep their -100 ID. Changing this retargets where posts go.`
  - public row (verbatim): `@handle changes on Telegram's side are not tracked here — update it here too.`
- **Confirm** (house `js_arg` + `esc` idiom, `channels.rs:305-308`):
  `Change where posts for {channel} go? This retargets every plan and post that uses it.`
- **POSTs to the existing `POST /channels/rename`** with `channel` (current)
  + `new` — both already first-class aliases in `ChannelRenameForm`
  (`channels.rs:600-604`). Everything the danger path needs already lives
  there: scope enforcement, the UNIQUE NOCASE collision error
  (`db.rs:5679-5683`), the NOCASE self-rename guard (`channels.rs:679-681`)
  and the `renamed=1` PRG. Prefilling `new` with the current identifier means
  an unchanged submit hits the guard, whose banner copy updates to:
  `Nothing changed — that's still the channel's address. Use Edit name to change its label.`
  (this replaces `channels.rs:67`; the pinned assertion is listed at-risk in
  §8).
- The plain `<details>` inside a row already has styling and a margin fix
  (`app.css:519-533`, `li details` at `:533`) — no new CSS.

### 5c. Route decision: NEW `POST /channels/name` — not `/channels/title`, not a rename-handler field

**Chosen: a dedicated minimal handler `channels_name` in `channels.rs` +
`.route("/channels/name", post(channels_name))` in `routes_channels`
(`mod.rs:251-255`). ≈ 40 lines including doc comment and form struct.**

```rust
#[derive(Deserialize)]
pub(super) struct ChannelNameForm {
    pub(super) channel: String,
    pub(super) name: Option<String>,
}

/// POST /channels/name sets a global channel's display label
/// (`channels.title`) via the existing Db::set_channel_title (db.rs:2031) —
/// the human label, never the dialed identifier (CHANNEL_ADD_REDESIGN_SPEC
/// §5a). Owner-only: global rows are owner data. The identifier is rewritten
/// only by POST /channels/rename (§5b), which is why this is a separate
/// route and not a rename-handler field.
#[tracing::instrument(skip_all)]
pub(super) async fn channels_name(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelNameForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Channels", "Channel names");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // set_channel_title no-ops on a missing row — say so instead of a false
    // success (existence probe precedent: bind-sender, channels.rs:743).
    if state.db.get_channel_seller_id(ch) != Some(0) {
        return channels_error(
            &state, &session,
            "That channel isn't in the global list — add it first.",
        );
    }
    let name = f.name.as_deref().map(str::trim).filter(|t| !t.is_empty());
    state.db.set_channel_title(ch, name);
    state.db.log_action(session.account_id, &session.role, "channel_name", "channel",
        state.db.get_channel_id(ch).unwrap_or(0), None);
    Redirect::to(&state.url(&format!(
        "/channels?named=1&channel={}", urlencode_query(ch)
    )))
    .into_response()
}
```

**Justification, in order of weight:**

1. **`POST /channels/title` is occupied.** `mod.rs:359` routes it to
   `channel_title` (`channel_ops.rs:264-281`) = Bot API `set_chat_title` —
   the TELEGRAM-side title. Reusing the path for the local label would
   shadow a live route with opposite semantics. (This is exactly the
   conflation this spec exists to kill.)
2. **Extending `channels_rename` is the wrong shape.** Its form already
   accepts 14 alias fields through `first_non_empty`
   (`channels.rs:589-627`) for bot-handler compatibility; threading a title
   write through that soup risks alias collisions and would fork the NOCASE
   guard, which must KEEP firing for identifiers (pinned,
   `tests/channels.rs:224-227`) but must never fire for names.
3. **PRG and copy diverge.** The address flow's banner says "renamed to"
   (pinned copy); the name flow needs `?named=1` →
   `Channel name saved.` New `ChannelsQuery` field `named: Option<String>`
   (`channels.rs:130-143`) + one banner arm in `channels_banners` (success,
   same wrapper as `?default=1`).
4. **Audit parity:** `bind-sender` writes a `log_action` row
   (`channels.rs:785-792`); a label edit gets the same treatment
   (`"channel_name"` action) — cheap, additive, consistent.
5. Impact: additive handler + one route line; the rename handler and
   its pins stay byte-identical.

---

## 6. Item 4 — Badge truth: confirmed, kept, one wrinkle named and fixed

- **Confirmed:** `channel_type_badge` (`channels.rs:416-422`) derives the
  badge from the STORED identifier's shape at render: `starts_with('@')` →
  `public`, else `private`. There is no visibility column and this spec adds
  none (no DB changes) — the radio is an input-time gate only; the badge
  stays render-derived. **Keep as-is.**
- **The wrinkle, named:** today the add flow stores the RESOLVED numeric id
  for a public channel whenever the bot answers getChat
  (`channels.rs:486` → `content.rs:242-266`), so a genuinely public channel
  can wear the `private` badge. §3.4.4 fixes it at the source — the public
  path stores `@handle` (normalized by `normalize_channel_input`,
  `content.rs:183-208`) — after which shape-derived truth holds everywhere:
  `@` → public, `-100` → private, matching the radio the user chose.
- **Residual edge (documented, no action):** rows created before this fix
  may hold a numeric id for a public channel and read `private` until
  retouched via Change address (§5b) or re-added. No migration (no DB
  changes). Hand renames `-100 → @handle` flip the badge public — correct
  and pre-existing (CHANNELS_TITLE_UX_SPEC §2.4).

---

## 7. Constraints for web (non-negotiable)

1. **Step 0 first, move-only** (§2): 161 lines out to `channels_rows.rs`,
   `mod.rs` wiring only. The redesign edits ride AFTER the move.
2. **No DB changes.** No schema, no migration, no new `Db` methods.
   `set_channel_title` (`db.rs:2031`) and `rename_channel` (`db.rs:5650`)
   are the only writers.
3. **Exactly one new route:** `POST /channels/name` (§5c). The rename route,
   add route, default/remove/bind-sender routes are untouched.
4. **`esc()` every dynamic value** — titles, identifiers, confirm strings
   (`js_arg` + `esc`), callout names, redirect params
   (`urlencode_query`). The `misc.rs` esc proptest is the guard; static
   copy is literal.
5. **Hidden inputs, `aria-label`s, and confirm() strings keep quoting the
   RAW identifier** (escaped) — never the display label
   (CHANNELS_TITLE_UX_SPEC §4.3 rule).
6. **Reuse house classes only** (`help`, `help-more`, `alert alert-info`,
   `badge*`, `mono`, `inline-form`, `btn btn-secondary/danger/ghost btn-sm`,
   `check`, `sr-only`). The ONLY new CSS is the §4 block (radio/fieldset +
   `.row-added`). No inline styles.
7. **Function bodies ≤ 100 lines** — the §2.2 decomposition covers the one
   pre-existing offender; every new handler/helper ships under budget.
8. **File budgets:** `channels.rs` ≤ 800 after everything (projected ≈ 754,
   §2.3); `channels_rows.rs` ≈ 201. If `channels.rs` exceeds 795 during
   implementation, use the §2.3 overflow valve before trimming any copy.
9. **A11y:** fieldset+legend groups the radios; visible radio labels;
   `aria-describedby` follows the adaptive help swap (§3.3); the focused row
   is `tabindex="-1"` + `data-focus-on-load` (never in tab order); AA
   contrast pairs only (DESIGN.md §4).
10. **Server is the validation authority** (§3.4); the radio/JS is
    enhancement. The page must work with JS disabled.
11. Prefer additive changes over edits to asserted markup; §8 lists every
    string tests pin today and its disposition. New tests assert with
    `contains`, never full-row equality.

---

## 8. Test risk: pinned strings, disposition, additive tests

### 8.1 At-risk assertions (verified against `tests/channels.rs`, `tests/misc.rs`, `smoke_tests.rs`)

| Location | Pinned string | Disposition under this spec |
|---|---|---|
| `tests/channels.rs:179` | `@handle, t.me link, or -100 ID — the bot must be admin in the channel.` (owner help one-liner) | **BREAKS** — replaced by the adaptive per-visibility help (§3.1). Update to pin `@handle or a t.me link — the bot must be admin in the channel.` |
| `tests/channels.rs:194` | same string (seller help one-liner) | **BREAKS** — same update (seller form §3.2) |
| `tests/channels.rs:183` | `<details class="help-more"><summary>Which forms are accepted?</summary>` | **SURVIVES** — summary kept verbatim (§3.1) |
| `tests/channels.rs:187` | `Adding one does not change the default — use Set default.` | **SURVIVES** — kept verbatim in the owner details |
| `tests/channels.rs:197` | `Adding one replaces the previous.` | **SURVIVES** — kept verbatim in the seller details |
| `tests/channels.rs:244-246` | `Nothing changed — that's still the channel's ID.` | **BREAKS** — guard copy updated to the address wording (§5b). Update the assertion |
| `tests/channels.rs:252-254` | `name="new" value="Tech Tips" placeholder=""` (title prefill rode the rename form) | **BREAKS** — Edit name is now `name="name" value="Tech Tips"` on `POST /channels/name` (§5a). Update |
| `tests/channels.rs:256-258` | `name="new" value="" placeholder="Friendly name, e.g. Tech Tips"` | **BREAKS** — becomes the Edit-name placeholder pin (`name="name" value="" placeholder="Friendly name, e.g. Wire Guard"`). Update |
| `tests/channels.rs:224-227` | same-guard redirect `/channels?renamed=1&same=1` | **SURVIVES** — Change address reuses `/channels/rename` + `channel`/`new` fields (§5b) |
| `tests/channels.rs:236` | `title unchanged` after no-op rename | **SURVIVES** — guard still writes nothing |
| `tests/channels.rs:260-272` | real rename → `/channels?renamed=1&channel=TechTips` | **SURVIVES** — rename handler untouched |
| `tests/channels.rs:24-32, 42, 49-50, 89, 97-98` | add redirects, page contents, `bot-managed`, default pins | **SURVIVE** — scripted posts send no `visibility` → legacy path (§3.4.1); seller page additions are markup-additive |
| `tests/channels.rs:149-159` | scoped empty-state copy | **SURVIVES** — empty-state `<li>` untouched |
| `tests/channels.rs:294-306` | dashboard composer options | **SURVIVES** — dashboard untouched |
| `smoke_tests.rs:166-181, 262-283` | `/channels` 200; `/channels/add` redirects | **SURVIVE** — status-only; no `Location` body asserted |
| `src/web/tests/misc.rs` | (none — no channel add/rename pins; esc proptest `:13-45`) | **NOT AT RISK** |
| `identity_tests.rs:859` | monitor `<option value="@testchan" selected>` | **SURVIVES** — no title/label change reaches monitor options (no test titles a channel) |

### 8.2 Additive tests to prescribe (all in `tests/channels.rs`, `contains` style)

1. **Visibility-dependent validation:** POST `/channels/add`
   `visibility=public&channel=t.me/wireguard` → redirect location contains
   `added=1` and the escaped handle; GET page shows `@wireguard` AND
   `>public<`. POST `visibility=private&channel=@nope` → redirect contains
   `error=private-id`; page renders the §3.4 fixed copy. POST
   `visibility=public&channel=-1001234567890123` → `error=public-shape`.
2. **Title saved via `set_channel_title`:** POST
   `visibility=public&channel=@foo&title=Wire Guard` → `list_channels` row
   title `Some("Wire Guard")`; page label `Wire Guard (@foo)`; and — user
   title wins — a `notitle`-less redirect even though getChat is unreachable.
3. **notitle honesty:** POST private `-100…` with NO title → redirect
   contains `notitle=1` and `channel=`; with a typed title → no `notitle`.
4. **Post-add reveal:** GET `/channels?added=1&channel=@foo` → body contains
   `row-added`, `tabindex="-1"`, `data-focus-on-load`, the callout fragment
   `is in.`, and NOT `Channel added.`; legacy `?added=1` alone still renders
   `Channel added.` (regression pin); `?added=1&notitle=1&channel=-100…`
   renders the notitle callout variant.
5. **Edit name route:** POST `/channels/name` `channel=@one&name=Tech Tips`
   → redirect `/channels?named=1&channel=@one`; page shows
   `Channel name saved.` and the updated label; seller session → 403
   (assert like `tests/channels.rs:125`).
6. **Guarded address edit:** row markup contains
   `Change address…`, the `-100` warning on a private row, and the
   `Retarget` danger submit to `/channels/rename`; the unchanged-submit guard
   still redirects `renamed=1&same=1` (existing test continues to cover it
   through the new form fields).

---

## 9. POST-ADD LIFECYCLE (owner-requested note for the plan/feature feed)

- **This wave:** the Title box makes the label a first-class input — typed
  title wins; empty title + bot-visible channel → getChat fills it
  (existing best-effort behavior, `channels.rs:486-511`). The callout (§4)
  closes the add with the one thing that actually blocks posting: the bot
  must be an admin in the channel.
- **Follow-up (optional, NOT this wave — needs the bot online): "Refresh
  title" row-action.** A `btn btn-ghost btn-sm` action next to Edit name
  that calls getChat for the stored identifier and writes
  `set_channel_title` (a tiny handler sibling to `channels_name`, PRG
  `?titled=1` → `Channel title refreshed from Telegram.`). This is the
  remedy for a real Telegram fact (§0.4): Telegram never notifies bots about
  title (or username) changes, so stored labels go stale silently. Prescribe
  now, schedule later; it must be honest when the bot is offline (fixed
  error copy, no silent success).
- **Related follow-ups:** (a) seller title capture — the seller branch
  computes a getChat title and discards it (`channels.rs:486-505`); storing
  it when the seller's channel coincides with a global row is a one-line
  candidate (CHANNELS_TITLE_UX_SPEC §8.1); (b) handle staleness — a public
  channel's @handle can change on Telegram's side with no notification; the
  Change-address control (§5b) is the manual remedy, and a future
  "re-resolve address" action would mirror Refresh title.

---

## 10. Acceptance checklist

- [ ] `channels.rs` ≤ 800 lines with `channels_rows.rs` extracted move-only
      first; `cargo build && cargo test && cargo clippy -- -D warnings &&
      cargo fmt --check` pass.
- [ ] Owner add form: Title box ("e.g. Wire Guard" placeholder),
      Public/Private radio (fieldset+legend), adaptive identifier field
      (`@handle` ↔ `-1001234567890123` placeholders + help swap + JS
      describedby swap), omnibus help collapsed in `help-more` with the
      kept tail sentences.
- [ ] Server validation: public accepts @handle/t.me → stores `@handle`;
      public refuses `-100…` (`error=public-shape`); private accepts `-100…`
      only (`error=private-id` otherwise); missing `visibility` → legacy
      behavior.
- [ ] Typed title beats getChat; `notitle=1` only when both are absent.
- [ ] Post-add: redirect `?added=1&channel=<id>`; row renders
      `.row-added` + `tabindex="-1"` + `data-focus-on-load`; callout after
      the list with role/owner-honest copy; top add banner suppressed when
      the callout renders; legacy redirects unchanged.
- [ ] Row actions: Edit name (safe, `POST /channels/name`, prefilled,
      `Channel name saved.` PRG) and Change address (danger, collapsed
      details, shape-branched warning, confirm, `POST /channels/rename`,
      guard intact).
- [ ] Badge still shape-derived at render (`channel_type_badge` unchanged);
      public adds store `@handle` so the badge is truthful.
- [ ] No DB changes; exactly one new route; every dynamic string `esc()`-ed;
      aria-labels/hidden inputs keep raw identifiers.
- [ ] Visual check light AND dark: radio group aligned on the 8px grid,
      `.row-added` reads as a calm tint (never loud), callout wraps at 375px.
