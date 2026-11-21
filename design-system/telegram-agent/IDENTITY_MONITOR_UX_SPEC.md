# Identity Monitor — Read-Only Timeline — UX & Accessibility Spec

**Owner:** design — spec only; the web layer implements in `src/web.rs`.
**Status:** v1 spec, from the ratified identity decision record (2026-10-27). WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** web implementation, server handlers/data, core data layer, security review, design token contract.
**Related:** `IDENTITY_UX_SPEC.md` (unlock banner, identity status line, wizard), `assets/app.css` (`.panel`, `.mono`, `.help`, `.empty`), `BOT_SWITCHING_UX_SPEC.md` (rail idiom), `docs/BOOKINGS_UX_SPEC.md` (routes/flash vocabulary, forgiving-URL rule).

---

## 0. Summary of decisions

1. **Read-only, stated everywhere.** `/monitor` renders recent channel history as seen by the personal account. The `h1` sub reads "read-only · viewing as your personal account"; the page has **exactly two forms, zero action buttons** — nothing on it can mutate anything, on the console or on Telegram.
2. **The timeline is a semantic `<ol>`** in a `.panel`: newest-first, 100-message cap, each entry a mono header (`{date} · {sender}`) above its body. Replies nest exactly one level, as an indented sub-list — no deeper tree, ever.
3. **Two GET forms only:** a channel picker (select + "View") and a search field ("Search"). Both submit GET to the same route; all state lives in the URL — shareable, refreshable, back-button honest.
4. **Pagination is a real link**, "Older messages" → `?before={message_id}` (preferred form, pending the pushdown spike, §9.2). No infinite scroll, no auto-refresh, no polling — the page is static between navigations.
5. **One new CSS rule block authorized** (§8): `.timeline` — the read-only list idiom. Everything else is existing primitives.

---

## 1. Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/monitor` | Timeline for the selected channel (`?ch={channel_id}`), or search results (`?q={query}`, optionally `?ch=` too), or an older page (`?before={message_id}`) | session required; owner only; identity connected **and unlocked** |

- Base-path prefix; unauthenticated → 303 `/login`; non-owner → `forbidden_page` (403). The route is in the owner-only Identity rail group and re-derives the role server-side.
- There are **no POST routes on this page** — read-only is enforced by the route table itself, not by hiding buttons.
- **Not connected:** the route renders the not-connected panel (§5) — the shell exists, the data fetch never happens.
- **Locked:** the route renders the locked empty state (§5) and fetches nothing. Reading as the personal account is an identity action and is server-gated like the rest.

---

## 2. Layout

Single column, `max-width: var(--measure)`, console shell. Top-to-bottom:

1. **Page head** — `h1 Monitor` + `.sub` "read-only · viewing as your personal account". The sub is a standing fact, rendered on every variant of the page (timeline, search results, older page) — the operator should never lose track of whose eyes they are looking through.
2. **Controls row** — the exactly-two forms (§3).
3. **Timeline panel** (or a state panel, §5) — `<article class="panel">` with `h3` naming the channel ("`@channel` · latest messages" / "Search results") and the `<ol class="timeline">`.
4. **Pagination row** — "Older messages" link, or a `.help` end-of-history line (§4.3).

### 2.1 The two forms (GET, same route)

```html
<form method="get" action="{base}/monitor" class="inline-form">
  <label for="monitor-channel" class="sr-only">Channel</label>
  <select id="monitor-channel" name="ch">…</select>
  <button type="submit" class="btn btn-secondary btn-sm">View</button>
</form>
<form method="get" action="{base}/monitor" class="inline-form">
  <label for="monitor-search" class="sr-only">Search messages</label>
  <input type="search" id="monitor-search" name="q" placeholder="Search messages">
  <button type="submit" class="btn btn-secondary btn-sm">Search</button>
</form>
```

- **Picker options:** the channels the personal account can read (membership as synced from Telegram), labeled by their `@handle`/name; the currently-viewed channel carries `selected`. No membership data yet → the picker renders with a single empty option and the panel says so (§5).
- **Forgiving URLs** (Bookings §3.1 rule): missing `ch` renders the picker prompt; unknown `ch` renders "That channel isn't available to your account." as an `alert-warn` + empty timeline; empty `q` is treated as no search and renders the plain timeline — never an error.
- Search and pagination compose: `?ch={id}&q={q}&before={id}` — an older page of the same search.
- Exactly two forms is a **cap**, not a target: no date-range form, no sender filter, no export form in v1.

---

## 3. Timeline

### 3.1 Structure

```html
<ol class="timeline" aria-label="Messages from {channel}, newest first">
  <li>
    <div class="mono">{date} · {sender}</div>
    <p>{message body}</p>
    <ol aria-label="Replies">          <!-- only when the message has replies -->
      <li>
        <div class="mono">{date} · {sender}</div>
        <p>{reply body}</p>
      </li>
    </ol>
  </li>
</ol>
```

- **Newest first**, capped at **100 messages** per render. When the cap clips, the footer link appears (§4.3); the count is stated in a `.help` line: "Showing the latest 100 messages."
- **Header line** `{date} · {sender}` in `.mono` — date as stored (UTC) and sender display name (or `user {id}` when the name is unknown; ids in mono are the console's figure convention).
- **Bodies** are plain text: `esc()`'d, whitespace preserved; media messages render as a one-line description ("[photo]" / "[document] {filename}") — the monitor reads conversation, not attachments, and must never fetch media as a side effect.
- **Nesting is exactly one level.** Replies to replies flatten into the same sub-list, in thread order. A second nesting level would triple the indentation logic, break the 100-cap accounting, and make screen-reader traversal ambiguous — the cap is a design decision, not a rendering shortcut.
- The sub-list exists only when there are replies; empty containers never render.

### 3.2 Authorized CSS (the one new block)

`.timeline` in `assets/app.css`:

```css
.timeline { margin: 4px 0 0; padding-left: 20px; }
.timeline li { border-bottom: 1px solid var(--line); padding: 10px 0; }
.timeline li:last-child { border-bottom: none; }
.timeline .timeline, .timeline ol { margin-top: 6px; padding-left: 18px; }
```

- Global CSS resets `ul` but not `ol`; without this block the timeline renders with browser-default numbering and no ledger dividers. This is the "unavoidable" case: list semantics require `<ol>`, the ledger look requires the dividers. It reuses existing tokens only (`--line`), no new colors, and mirrors the shipped `article ul li` divider rhythm.

---

## 4. Pagination & search results

### 4.1 "Older messages" is a real link

`<a href="{base}/monitor?ch={id}&before={first_message_id_on_page}">Older messages</a>` — a normal anchor: middle-clickable, copyable, works without JS, and a navigation that screen readers announce.

- `before` is the **oldest message id on the current page**, so the next page starts strictly before it — no overlap, no gaps if messages arrive between clicks.
- `?before=` is the preferred cursor form **pending the spike** (§9.2): if the Telegram layer cannot page server-side by id, the link pages the synced cache instead and this section's URL shape is revisited before ship — flagged, not silently changed.

### 4.2 Search results

- The same timeline component, same markup, same cap — search results are not a second UI.
- Panel `h3` reads "Search results"; a `.help` line states scope honestly: "Matches in {channel} · newest first · latest 100." or "Matches in all channels · newest first · latest 100." when no `ch` is given.
- No match highlighting in v1 — highlighting is presentation logic on untrusted text and buys nothing for a read-only check.

### 4.3 End of history

When a page returns fewer than the cap with no older page available, the pagination row is a `.help` line: "That's the oldest message available." — an end, not an error.

---

## 5. State panels (not connected / locked / no channels)

All three render the full page shell (head, sub, controls where meaningful) with one `.empty` panel — the state is the content:

| State | Panel |
|---|---|
| No identity connected | `<p class="empty"><b>No personal identity connected.</b> Connect one on the Identity page to read as yourself.</p>` + a link to `{base}/identity`. No controls row (nothing to pick yet). |
| Identity locked | `<p class="empty"><b>Your identity is locked.</b> Unlock it in the banner above to read as your personal account.</p>` — points at the standing unlock banner (IDENTITY_UX §4). |
| Connected but no readable channels | `<p class="empty"><b>No channels to read yet.</b> Channels your personal account joins appear here.</p>` — the picker renders with its empty option. |
| Zero search matches | `<p class="empty"><b>No matches.</b> Try fewer words, or search all channels.</p>` + link to clear (`{base}/monitor`). |

---

## 6. WCAG 2.1 AA floor (non-waivable)

1. **List semantics.** The timeline is a real `<ol>` with an `aria-label` naming the channel and the order ("newest first"); replies are a nested `<ol>` with their own label. Screen readers announce position and count natively — no fake list markup, no `div` rows.
2. **Long-list traversal.** One `<h1>`, panel `h3`s, and the mono header line per message give keyboard and screen-reader users three stable landmarks. 100 items is a scroll, not a maze: the pagination link is a normal focusable anchor at the end, and every navigation is a full page load (document re-announcement, focus at top).
3. **Labels.** Both form controls carry programmatic labels (sr-only, §2.1); buttons have text.
4. **Contrast.** All pairs shipped: `.mono` header `--ink-2`-adjacent on surface, body `--ink`, `.help` `--ink-3`, `.timeline` dividers `--line` (non-text decorative). No new colors beyond reusing `--line`.
5. **No color-only state.** Sender, date, and read-only posture are all text; nothing on the page encodes meaning in color at all.
6. **Focus.** Native `:focus-visible` ring on links, select, input, buttons; no `outline: none`; no positive `tabindex`; no focus trapping (there is no widget to trap).
7. **Motion / refresh.** No auto-refresh, no polling, no animation — safe under `prefers-reduced-motion` by construction; the timeline never reorders under the reader.
8. **Touch.** The "Older messages" link and both `.btn-sm` controls keep ≥ 44px hit height at ≤ 480px.

---

## 7. Security & privacy

1. **Read-only by construction.** No POST routes, no mutation handlers — tests can confirm read-onlyness from the route table alone (§1).
2. **Owner-only.** The personal account's channel history is the most sensitive surface in the product; role re-derived server-side on every request, never from the UI.
3. **Locked = no fetch.** The data fetch sits behind the same `identity_locked()` gate as every identity action (IDENTITY_UX §4) — a locked identity must not silently render from a stale cache either: the locked state panel is the whole content.
4. **No channel content in logs or URLs beyond the cursor** — `?q=` rides the query string by design (GET search); it must be excluded from request logging (same posture as SETUP_MCP §6.8). **Security verifies.**
5. **Escaping.** Every message body, sender name, and channel name passes `esc()`; badge/h3 strings are fixed templates.
6. **No media fetches.** §3.1's one-line media descriptions keep the page from pulling attachments (bandwidth, NSFW-on-screen, and cache side effects all avoided).

---

## 8. Token-contract flags (design)

**One new CSS rule block, authorized:** `.timeline` (§3.2) — list reset + ledger dividers + one nesting indent, existing tokens only. Rationale and the no-CSS alternative (browser-default `<ol>` inside a panel) documented in §3.2; the alternative fails the ledger look and buys nothing. Everything else — `panel`, `mono`, `help`, `empty`, `btn-secondary btn-sm`, `inline-form`, `alert-warn`, sr-only — is shipped. No new components, no inline styles, no new color values.

---

## 9. Backend contracts web/server/core must satisfy (flags, not fixes)

1. **Synced channel history per identity** — a read model the page can page through (message id, date, sender, body, reply-to). **Owner: core (storage), server (sync).**
2. **`?before` pushdown spike.** Whether Telegram's API can page server-side by message id, or the console pages its own synced cache. The §4.1 link shape assumes pushdown; if the spike fails, the URL semantics change before ship — decided, not drifted. **Owner: server, with design on the URL shape.**
3. **Search source** (the carried follow-up): Telegram-side search vs search over the synced cache. Same spike, same decision discipline (§10.1).
4. **Read lock check** — the fetch is behind `identity_locked()`; the locked render is server-side, not a client-side courtesy. **Owner: web/server.**

---

## 10. Acceptance checklist (for web / verification)

- [ ] `/monitor` is owner-only; not-connected and locked renders show the §5 state panels and perform no data fetch.
- [ ] The page contains exactly two forms (picker, search), both GET to `/monitor`; zero buttons that POST anywhere.
- [ ] `h1` sub reads "read-only · viewing as your personal account" on every variant.
- [ ] Timeline is an `<ol>` with mono `{date} · {sender}` headers; newest-first; 100-message cap; media renders as one-line descriptions.
- [ ] Replies nest exactly one level as an indented `<ol>`; deeper replies flatten; empty sub-lists never render.
- [ ] "Older messages" is a real link with `?before={oldest id on page}`; pages don't overlap; end-of-history is a help line.
- [ ] Search results render as the same timeline with an honest scope line; empty `q` is the plain timeline; unknown `ch` is a warn panel, not an error.
- [ ] No auto-refresh, no polling, no animation; no channel content or `q` in logs.
- [ ] `.timeline` is the only new CSS; everything else uses shipped primitives.

---

## 11. Follow-ups (non-blocking)

1. **Search: server-vs-synced (spike, carried).** If Telegram-side search wins, results reflect Telegram's ranking and rate limits (a FloodWait surface for search joins the wizard's, with the same honest UTC copy); if synced-cache search wins, freshness becomes the honest caveat ("search covers messages up to {date}"). Either way the UI copy in §4.2 gets one scope line updated — the timeline component does not change.
2. **Jump-to-date.** A third form would break the exactly-two cap; if product wants it, it arrives as a URL pattern (`?at={date}`) shared as a link, not a form — v2.
3. **Per-message anchors.** `#m{message_id}` deep links into the timeline for support screenshots — cheap, but needs scroll-position + focus handling to stay AA; deferred so v1 ships with no half-done focus management.

---

*End of spec.*
