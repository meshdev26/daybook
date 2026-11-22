# Identity Sender — Channel Binding & Attribution — UX & Accessibility Spec

**Owner:** design — spec only; the web layer implements in `src/web.rs`.
**Status:** v1 spec, from the ratified identity decision record (2026-10-27). WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** web implementation, server handlers/data, core data layer, security review, design token contract.
**Related:** `IDENTITY_UX_SPEC.md` (Enable-posting gate, unlock banner, wizard), `assets/app.css`, `src/web.rs` (`channels_body` row idiom, `posts_page`, dashboard composer, `keys_body` confirm idiom), `docs/BOOKINGS_UX_SPEC.md` (flash + `confirm()` vocabulary).

---

## 0. Summary of decisions

1. **Binding is a row action on the existing Channels page** — no new page. Bot-bound rows gain "Post as user…" (`btn-ghost btn-sm` + native `confirm()` naming the channel and the risk); user-bound rows gain "Post as bot" (no confirm) and carry a `badge badge-warning` "as user".
2. **Exception-only marking.** Bot-bound rows are unmarked — posting as the bot is the product's default and needs no label. Only the risky, exceptional state (posting as the personal account) is marked, everywhere it can be seen.
3. **The first user-binding is gated by the Enable-posting acceptance** (`IDENTITY_UX_SPEC` §5). The server rejects a binding POST while the gate is unset, and the flash points at that gate — the console never re-asks the ToS question a third time.
4. **Attribution follows the binding.** Post lists append the same "as user" badge to the Channel cell; the dashboard composer shows the badge plus a one-line pre-send help whenever its target channel is user-bound.
5. **No new tokens, no new CSS.** The page is buildable from `badge badge-warning`, `btn-ghost btn-sm`, `inline-form`, `alert*`, `help`, `mono` — see §9.

---

## 1. Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/channels` | Existing page; rows gain the binding badge and (owner only) the binding row-actions | session required (owner manages the global set; seller sees own — existing rules unchanged) |
| POST | `/channels/bind-user` | Bind one channel to post via the personal account | session; owner only; requires the posting gate + unlocked identity |
| POST | `/channels/bind-bot` | Revert one channel to bot posting | session; owner only; requires unlocked identity |

- Base-path prefix; unauthenticated → 303 `/login`; non-owner POST → `forbidden_page` (403). Sellers and admins see the "as user" badge as a fact but get no binding buttons — the identity is the owner's personal account, so binding is an owner decision.
- Both POSTs write `log_action` audit rows (`channel_bind_user` / `channel_bind_bot`, detail = channel id).
- Attribution surfaces (`/posts`, dashboard) are render-time only — no new routes there.

---

## 2. Channel rows (on `/channels`)

Each row's action cell gains one `inline-form` (hidden `channel_id`, base-prefixed POST), decided by the row's current binding:

| Row state | Marker | Row action | Confirm? |
|---|---|---|---|
| Bot-bound (default) | none | **Post as user…** — `btn btn-ghost btn-sm` | **Yes** — native `confirm()` |
| User-bound | `<span class="badge badge-warning">as user</span>` at the row's channel name | **Post as bot** — `btn btn-secondary btn-sm` | No |

### 2.1 Confirm copy (verbatim)

The `confirm()` message must name the channel and the risk:

```
Bind {channel} to your personal account? Posts to this channel will be sent as you — not as the bot — and every member sees them as yours.
```

- `{channel}` is the row's rendered channel name/`@handle`, plain text. One sentence, one decision; no second paragraph of legalese — the full risk copy lives once, behind the Enable-posting gate it triggers (`IDENTITY_UX_SPEC` §5).
- The ellipsis in "Post as user…" marks that the button opens a question, not an action.
- Reverting to the bot is the safe direction: no confirm (the same asymmetry as Bookings Confirm/Decline).

### 2.2 Accessible names

- `aria-label="Post to {channel} as your personal account"` on the bind-user button.
- `aria-label="Post to {channel} as the bot"` on the revert button.
- Never bare "Post as user…" — virtual-buffer users must know which row they act on (BOOKINGS §3.5 rule).

---

## 3. Gate, lock, and failure behavior

| Case | Result |
|---|---|
| Bind-user while the posting gate is unset | Rejected server-side; redirect `/channels?error=posting` → `alert-error` / `role="alert"`: "Posting as your personal account isn't enabled yet. Enable it on the Identity page first." — "Identity page" links to `{base}/identity`. Nothing bound. |
| Bind-user or bind-bot while the identity is locked | Rejected; `/channels?error=locked` → `alert-error`: "Your identity is locked — unlock it in the banner above." The binding (a stored fact) survives; only the mutation is refused. |
| Success (either direction) | PRG with `?bound-user=1` / `?bound-bot=1` → `alert-success` / `role="status"`: "Posts to {channel} will be sent as your personal account." / "Posts to {channel} are sent by the bot again." |

Rules:

- The badge and buttons are affordances; the gate and the lock are enforced in the handler, never inferred from the UI.
- **Honesty while locked, on every path:** a user-bound send that fails because the identity is locked (scheduled post firing, immediate send) must surface as an explicit failure on its own surface — never silence, never a bot-post fallback. If the lock lands mid-schedule, the post fails loudly with "identity locked" as the reason. **Server contract; verification confirms no silent-drop path.**
- A channel deleted or removed while user-bound simply loses its row; no orphan cleanup surface in v1.

---

## 4. Attribution in post lists (`/posts`)

The Channel cell of every post row appends the marker when the post was or will be sent via the personal account:

```html
<td>@channel <span class="badge badge-warning">as user</span></td>
```

- Same word, same badge, as the Channels row — one vocabulary for one fact.
- Scheduled and sent rows are marked alike: the badge describes the *sender* the row is bound to, not the delivery state (delivery state keeps its own badge).
- Bot-bound rows get nothing — exception-only marking (§0.2).
- The badge never carries the channel name; it sits in the Channel cell, directly after it, so the pair reads as one fact.

---

## 5. Dashboard composer attribution

When the composer's selected target channel resolves user-bound, the composer shows, **pre-send** (before any POST):

- The same `<span class="badge badge-warning">as user</span>` beside the target channel.
- One `.help` line directly under the target selector: "This will post to @channel as your personal account."

Rules:

- The help line re-renders with the target selection (full-page form semantics — it is computed server-side from the current selection; no JS requirement).
- It appears **only** when the target is user-bound; a bot-bound target shows nothing (exception-only marking again — a help line that is always there teaches nobody anything).
- If the target channel is user-bound **and** the identity is locked, the line becomes: "This will post to @channel as your personal account — which is locked. Unlock it in the banner above." The server still rejects the send; the line just removes the surprise.
- The badge/help pair is the pre-send disclosure; the send itself needs no confirm (the disclosure is persistent, and a confirm on every send would train the operator to click through it).

---

## 6. WCAG 2.1 AA floor (non-waivable)

1. **Labels & names.** Binding buttons carry channel-named `aria-label`s (§2.2); the confirm text is plain text read natively by `confirm()`.
2. **Keyboard.** Both row actions are native buttons in `inline-form`s — tab-reachable, Enter-actuated; `confirm()` is keyboard- and screen-reader-accessible (shipped idiom). No JS beyond the native dialog.
3. **Contrast.** `badge-warning` is the shipped `--warn` on `--warn-tint` pair; `btn-ghost` is the shipped ink-2-on-surface pair. No new colors.
4. **No color-only state.** The binding is a word ("as user") in a badge plus explicit button labels; the dot is decorative. A color-blind operator reads the same facts as everyone else.
5. **Announcements.** Success flashes `role="status"`; gate/lock errors `role="alert"`; full-page PRG renders re-announce the document naturally.
6. **Focus.** Banner-bearing renders move focus to the banner (`tabindex="-1"`, the IDENTITY_UX §4 idiom). No `outline: none`, no positive `tabindex`.
7. **Touch targets.** Row-action `.btn-sm` keeps ≥ 44px hit height at ≤ 480px (grow padding on small screens, per the Bookings §3.5 rule).
8. **Motion.** No animation; no auto-refresh; the table is static between navigations.

---

## 7. Security & privacy

1. **Server-side gates.** Owner-only POSTs; posting-gate and lock checks re-derived from the session inside the handler — a forged or replayed form changes nothing it shouldn't.
2. **Audit.** Both POSTs write `log_action` rows with the channel id; the bind-user row is the audit trail for "why does Telegram show this post as the owner".
3. **Confirm is a UX guard, not a security boundary** — the server accepts and rejects regardless of what the dialog said; the dialog exists so the operator never binds by accident.
4. **Escaping.** `{channel}` passes through `esc()` in every banner, button label, and confirm string; badge classes come from the fixed allow-list.
5. **No new PII in URLs** — only `channel_id` and flash keys ride the query string.
6. **CSRF** — existing SameSite=Strict posture; no new scheme.

---

## 8. Backend contracts web/server must satisfy (flags, not fixes)

1. **Binding state per channel** — one stored fact (bot | user) the console reads; default `bot` for every existing channel so nothing changes behavior on deploy. **Owner: core (storage), server (enforcement).**
2. **Send path honors the binding** — the user-bound send goes through the identity client; while locked it fails loudly (§3). **Owner: server.**
3. **Dashboard target resolution** — the composer's target channel id must resolve to its binding server-side each render. **Owner: web/server.**
4. **Flash keys** — `bound-user`, `bound-bot`, `error=posting`, `error=locked` on `/channels`. **Owner: web.**

---

## 9. Token-contract flags (design)

**None.** No new CSS, no new classes, no inline styles, no new color values. Everything reuses: `badge badge-warning`, `btn`/`btn-secondary`/`btn-ghost`/`btn-sm`, `inline-form`, `alert`/`alert-success`/`alert-error`, `help`, `mono`, and the native `confirm()` idiom from `keys_body`. The ellipsis in "Post as user…" is text, not a new component.

---

## 10. Acceptance checklist (for web / verification)

- [ ] Bot-bound rows (owner): "Post as user…" ghost button + confirm naming the channel and the risk; user-bound rows: "as user" badge + "Post as bot" with no confirm.
- [ ] Bot-bound rows carry no marker anywhere (channels, posts, dashboard).
- [ ] Bind-user with the posting gate unset: rejected, `error=posting` flash naming the Identity page, nothing bound.
- [ ] Binding mutations while locked: rejected with `error=locked`; existing binding unchanged.
- [ ] Both POSTs are owner-only (seller/admin POST → 403) and write audit rows.
- [ ] Post-list Channel cells show the "as user" badge exactly when the row's channel is user-bound.
- [ ] Dashboard composer shows the badge + pre-send help only for a user-bound target; locked variant names the lock.
- [ ] A user-bound send while locked fails loudly on its own surface — no silent drop, no bot fallback.
- [ ] All `aria-label`s name the channel; flashes use the §3 vocabulary; badges are `badge-warning`.

---

## 11. Follow-ups (non-blocking)

1. **Broadcast page inherits the same pre-send help** when it grows per-channel targeting — same badge, same one-line disclosure, same exception-only rule. Not in v1 because Broadcast has no channel picker today.
2. **Bulk binding** (bind several channels at once) — deferred; the one-row confirm copy does not generalize to N channels, and a bulk-risk sentence should be written deliberately, not squeezed into this one.
3. **Binding history in the post row** — whether a *sent* post should record which sender it used (fact at send time) rather than deriving from the current binding; matters only once channels can be re-bound often. server/core to size.

---

*End of spec.*
