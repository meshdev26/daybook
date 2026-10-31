# Identity Inbox — Pairing, Allowlist & Replies — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec only; L7 implements in `src/web.rs`.
**Status:** v1 spec, from the ratified identity decision record (2026-10-27). WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L9 (data layer), L10 (security review), L6-C (token contract).
**Related:** `IDENTITY_UX_SPEC.md` (unlock banner, identity status line), `IDENTITY_MONITOR_UX_SPEC.md` (`.timeline` list idiom), `docs/SETUP_MCP_UX_SPEC.md` (shown-once secret idiom, TTL copy), `docs/BOOKINGS_UX_SPEC.md` (routes/flash/confirm vocabulary), `assets/app.css`.

---

## 0. Summary of decisions

1. **Three stacked panels:** thread list (`.table-wrap`: From · Last message · Status) · thread view + composer (`?t={user_id}`) · Allowlist table (user_id · paired · Remove with `confirm()`). A fourth, conditional **Blocked panel** sits directly under the thread list when un-allowlisted first contacts exist.
2. **Pairing is the only door.** "Generate pairing code" → an 8-char code in `code.copyable`, **shown once** → the operator hands it to the buyer → the buyer DMs it to the personal account → a pending row appears with Approve / Deny. There is **no compose-first affordance anywhere**: no "New message" button on this page or any other; replies exist only inside a thread that exists because someone was allowlisted.
3. **First contact without pairing is blocked by default.** The Blocked panel states exactly what happened — "Not on the allowlist. Nothing was sent to them." — and offers one action: **Allow**.
4. **The composer is rate-honest.** Labeled textarea + "Send reply" + a visible "Replies today {n}/30" meter; at the cap the server rejects with `role="alert"` and **the draft is preserved** in the re-rendered textarea.
5. **No new tokens, no new CSS.** Buildable from `table-wrap`, `badge*`, `btn*`, `inline-form`, `alert*`, `code.copyable`, `empty`, `help`, `mono`, `sr-only`, and the `.timeline` idiom authorized in the Monitor spec — see §9.

---

## 1. Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/inbox` | Thread list panel + allowlist + (conditional) blocked + pairing panel | session required; owner only |
| GET | `/inbox?t={user_id}` | Thread view + composer for one contact | session; owner only; contact must be allowlisted |
| POST | `/inbox/reply` | Send one reply from the composer (30/day cap) | session; owner only; unlocked |
| POST | `/inbox/pair` | Generate a pairing code (invalidates the previous one) | session; owner only |
| POST | `/inbox/approve` | Approve a pending pairing → allowlist the contact | session; owner only; unlocked |
| POST | `/inbox/deny` | Deny a pending pairing (row leaves the list) | session; owner only |
| POST | `/inbox/allow` | Allow a blocked first contact → allowlist them | session; owner only; unlocked |
| POST | `/inbox/remove` | Remove a contact from the allowlist (confirm-first) | session; owner only |

- Base-path prefix; unauthenticated → 303 `/login`; non-owner → `forbidden_page` (403). The page lives in the owner-only Identity rail group; every route re-derives the role from the session — the buttons are affordances, not the gate.
- While the identity is **locked**, the mutating routes (`reply`, `pair`, `approve`, `allow`) are rejected server-side with the shared `?error=locked` flash pointing at the standing unlock banner (IDENTITY_UX §4). Reading the inbox (thread list, thread view) stays available — the operator can triage while locked, they just cannot send.
- Every mutating POST writes a `log_action` audit row (`inbox_reply`, `inbox_pair`, `inbox_approve`, `inbox_deny`, `inbox_allow`, `inbox_allowlist_remove`; detail = contact `user_id`).

---

## 2. Panel 1 — Thread list

`.table-wrap` + `<table aria-label="Conversations">` with `scope="col"` headers and an sr-only caption ("Conversations with last message and allowlist status").

| # | Column | Content |
|---|---|---|
| 1 | From | Buyer display name if known, else `user {id}` in `.mono`; the cell is a link to `?t={user_id}` |
| 2 | Last message | Snippet of the most recent message, one line, `esc()`'d, truncated with an honest ellipsis at ~80 chars |
| 3 | Status | Badge word: `badge-success` `allowed` · `badge-warning` `pending` · neutral `blocked` — word + state, never color alone |

- Row order: most recent activity first (the list answers "who talked last").
- The From cell is the only link; rows are not wholly clickable (table rows that link are a keyboard trap in the making — one clear link per row).
- Empty state: `<p class="empty"><b>No conversations yet.</b> Generate a pairing code below and give it to a buyer — that starts the first thread.</p>`

---

## 3. Panel 2 — Blocked first contacts (conditional)

Rendered directly under the thread list, only when at least one non-allowlisted user has DMed the personal account:

- `article.panel` + `h3 Blocked` + one `.help` line: "These people messaged your personal account but aren't on the allowlist."
- Each row: sender name/`user {id}` (`.mono`) · the inbound text (snippet, read-only) · `<span class="help">Not on the allowlist. Nothing was sent to them.</span>` · **Allow** (`inline-form` → `/inbox/allow`).
- The sentence is the spec: it tells the operator the console **did not auto-reply, did not acknowledge, did not leak presence**. It renders verbatim per row — this is the panel's whole job.
- **Allow** = add to allowlist + PRG to `/inbox?t={user_id}` with `?allowed=1` ("{contact} added to the allowlist. You can reply now."). The original message is kept; the operator answers it themselves in the composer — nothing is ever sent automatically.
- No "Deny" here: doing nothing is the denial (the row persists, harmless); removal vocabulary is reserved for the allowlist. If the panel grows long, it caps at 20 rows with a footer count — flagged as a v2 trim, not silently dropped.

---

## 4. Panel 3 — Thread view + composer (`?t={user_id}`)

### 4.1 Thread view

- `article.panel` + `h3` naming the contact ("Conversation with {contact}").
- Messages render **oldest-first** in the authorized `.timeline` `<ol>` (Monitor §3.2) — a conversation reads top-down and the composer sits below it. Direction is text, not color: each entry's mono header is `{date} · {sender}` where the sender name distinguishes "you" from the contact. (The thread list stays newest-activity-first; only the conversation itself is chronological.)
- Same body rules as Monitor: plain text, `esc()`'d, media as one-line descriptions.
- Thread cap 100 per render with a `.help` line and — since replies are short — typically absent: "Showing the latest 100 messages." No pagination link in v1 (follow-up §10.2 if threads grow).

### 4.2 Composer

```html
<form method="post" action="{base}/inbox/reply">
  <input type="hidden" name="user_id" value="{user_id}">
  <label for="inbox-reply">Reply to {contact}</label>
  <textarea id="inbox-reply" name="text" required></textarea>
  <span class="help" id="inbox-meter">Replies today {n}/30</span>
  <button type="submit" class="btn">Send reply</button>
</form>
```

- The label names the human ("Reply to {contact}") — not "Message" — so the accessible name says what the send does.
- **The meter is visible text**, adjacent to the field it governs, associated via `aria-describedby="inbox-meter"`. When remaining replies drop to 5 or fewer, the meter line becomes `<span class="badge badge-warning">Replies today {n}/30</span>` — the word and number carry the meaning; the tint only draws the eye.
- **At the cap the server rejects** — the composer is re-rendered with `alert-error` / `role="alert"`: "You've sent 30 replies today — that's the daily limit. Your reply is below, unsent." — and **the draft is preserved verbatim in the textarea**. A cap must never eat someone's typed words; this is the page's one hard rule about failure.
- The cap is 30 replies per rolling UTC day, counted server-side; the meter reads the same source. Client-side counting would be a lie waiting to happen.

---

## 5. Panel 4 — Pairing

### 5.1 Pairing panel

`article.panel` + `h3 Pair a buyer` + `.help` "Pairing is how a buyer gets a thread with you. They send the code to your personal account in Telegram; you approve it here."

- No active code and no pending rows → one button: **Generate pairing code** (`inline-form` → `/inbox/pair`).
- Active code → `code.copyable` block with the 8-char code, `.help` lines: "Valid for 15 minutes. Shown once — copy it now." and "One code at a time: generating a new one invalidates this one." Regenerate stays available as `btn btn-ghost btn-sm`.
- The shown-once rule is the SETUP_MCP/`keys_body` idiom: after any navigation the code is gone from the DOM; the DB keeps only its hash. A refreshed page that still shows the code would quietly turn a secret into a bookmark.

### 5.2 Pending approvals

When the buyer DMs the code to the personal account, the allowlist table gains a **pending row** (Status: `badge-warning` `pending`) whose action cell holds:

- **Approve** (`btn btn-sm`, no confirm — it is the time-critical, non-destructive direction; the Bookings Confirm asymmetry) → PRG `?approved=1`: "{contact} approved — they're on the allowlist. The thread opens below."
- **Deny** (`btn btn-ghost btn-sm`, no confirm — nothing exists yet to destroy; the request simply expires from the list) → PRG `?denied=1`: "Request denied. They can request again with a new code."
- Both buttons carry `aria-label`s with the contact id: "Approve {contact}" / "Deny {contact}".
- The pairing code is single-use and single-active: one live code per owner, consumed by whichever buyer sends it first (§7, §10.3).

---

## 6. Panel 5 — Allowlist

`.table-wrap` + `<table aria-label="Allowlist">` — columns: **user_id** (`.mono`, full value — the operator needs it for support) · **paired** (`.mono` date of approval) · **Remove**.

- Remove = `inline-form` with the native confirm, channel/named per the Bookings idiom, message naming the contact: "Remove {contact} from the allowlist? They won't be able to message you, and the thread stays read-only." — `btn btn-ghost btn-sm`, `aria-label="Remove {contact} from the allowlist"`.
- After removal the thread remains readable (history is history); the contact moves to the Blocked panel if they write again. Removal never deletes messages.
- Empty state: `<p class="empty"><b>Allowlist is empty.</b> Approve a pending pairing, or allow a blocked contact below.</p>` (worded to whichever panels exist).
- The allowlist is the single source of truth for who may hold a thread; the pending and blocked states are rows awaiting this table, not parallel lists.

---

## 7. WCAG 2.1 AA floor (non-waivable)

1. **Labels.** Composer textarea: programmatic `<label for>`; pairing/allow/pending buttons: text + contact-named `aria-label`s; table controls: `scope="col"` + sr-only captions (§2, §5.2, §6).
2. **Tables.** Real `<table>` semantics inside `.table-wrap`; the From cell is a real link with the contact as its text — never a JS row-click.
3. **Status is never color-only.** `allowed` / `pending` / `blocked` are badge words; the meter is a sentence with numbers; the blocked fact is a sentence. Tints are secondary.
4. **Announcements.** Success flashes `role="status"`; cap/lock errors `role="alert"`; focus moves to the banner on any error render (`tabindex="-1"` idiom). PRG navigations re-announce the page naturally.
5. **Contrast & focus.** Shipped token pairs only (`badge-warning`, `.help`, `alert-*`); native `:focus-visible` ring everywhere; no `outline: none`; no positive `tabindex`.
6. **Keyboard.** Every action is a native button or link in an `inline-form`; `confirm()` is keyboard/screen-reader accessible; the code.copyable block is `user-select: all` (one keystroke grabs it) with a visible focus ring on the block itself.
7. **Touch.** `.btn-sm` row actions keep ≥ 44px hit height at ≤ 480px; the code block's tap-to-select area is the full block.
8. **Motion.** No animation, no auto-refresh, no polling — the inbox is static between navigations; new messages appear on the next render, and nothing reorders under the reader.

---

## 8. Security & privacy (L10 flags)

1. **The pairing code is a bearer secret, shown once.** 8 chars of `gen_token`, hashed at rest, TTL 15 minutes, single-use, single-active per owner; generation and consumption are audited. **Owner: L8/L9; L10 confirms the TTL + hash posture.**
2. **Deny-by-default contact.** The Blocked panel's promise ("Nothing was sent to them") is an L3-testable invariant: no auto-ack, no auto-reply, no read receipt from any non-allowlisted contact path. **Owner: L8.**
3. **Approve/Allow are the only doors to the composer.** `?t=` for a non-allowlisted contact renders the thread list with the `error=unknown` flash — never the thread, never an oracle about whether the id exists.
4. **The 30/day cap is enforced server-side** on the identity client path; a forged or replayed composer POST cannot exceed it. **Owner: L8.**
5. **Audit without content.** `log_action` rows carry ids and actions, never message text or pairing codes.
6. **Escaping.** Snippets, names, codes via `esc()`; badge classes from the fixed allow-list.
7. **CSRF** — existing SameSite=Strict posture; no new scheme.
8. **Rate cap is also an anti-spam control for the owner's account** — Telegram limits accounts that mass-DM; the meter frames the cap as the product protecting the operator, and the copy never calls it an error of theirs.

---

## 9. Token-contract flags (L6-C)

**None.** No new CSS, no new classes, no inline styles, no new color values. Reuses: `table-wrap`, `badge`/`badge-warning`/`badge-success`, `btn`/`btn-ghost`/`btn-sm`, `inline-form`, `alert`/`-success`/`-error`, `code.copyable`, `empty`, `help`, `mono`, `sr-only`, and `.timeline` (already authorized in `IDENTITY_MONITOR_UX_SPEC.md` §3.2 — this spec adds nothing to it).

---

## 10. Acceptance checklist (for L7 / L3 verification)

- [ ] `/inbox` (owner only) renders: thread list, Blocked panel (only when blocked contacts exist), thread view (only with `?t=`), pairing panel, allowlist — in that order.
- [ ] Thread list columns From · Last message · Status; status is a badge word; From is the only link per row.
- [ ] Blocked panel rows read "Not on the allowlist. Nothing was sent to them." verbatim, with Allow as the only action; no auto-reply exists on any blocked path (L3 invariant).
- [ ] No compose-first affordance exists anywhere in the console — grep for "New message" finds nothing.
- [ ] Pairing: generate → 8-char code in `code.copyable`, shown once, TTL + single-active help lines; regenerate invalidates; a second render never shows the code.
- [ ] Pending row: Approve (no confirm) / Deny (no confirm), both with contact-named `aria-labels`; both audited.
- [ ] Allowlist: user_id · paired · Remove with confirm naming the contact; removal keeps the thread readable.
- [ ] Composer: labeled textarea named "Reply to {contact}", visible "Replies today {n}/30" meter, warning tint at ≤ 5 remaining; cap rejection is `role="alert"` with the draft preserved in the textarea.
- [ ] While locked: reply/pair/approve/allow rejected server-side with `error=locked`; thread list and thread view stay readable.
- [ ] `?t=` for a non-allowlisted or unknown id never renders a thread and leaks no existence oracle.
- [ ] No message text or pairing code in any log line or URL; no new CSS beyond the Monitor spec's `.timeline`.

---

## 11. Follow-ups (non-blocking)

1. **Pairing TTL + single-active — carried.** 15 minutes, one live code per owner, single-use, hashed at rest is this spec's assumption; L9 owns the storage constants and L10 the review. If the TTL moves, §5.1's help copy moves with it — the two must never drift.
2. **Thread pagination.** Threads beyond 100 messages have no older-page link in v1 (replies are short; the cap is generous). If real threads hit it, reuse the Monitor `?before=` idiom inside `?t=` rather than inventing a second pagination vocabulary.
3. **Blocked-panel trim.** The 20-row cap with a footer count (§3) is a v1 guardrail; a proper "show more" needs its own focus management pass.
4. **Unread emphasis.** Which threads have unseen messages is a product question (presence rules touch L10's "never reveal user presence" floor); deferred until that ruling exists.
5. **Buyer-side pairing copy.** What the buyer is told when they DM the code (bot-side reply) belongs in the L8 bot-copy pass — this spec owns the console half only.

---

*End of spec.*
