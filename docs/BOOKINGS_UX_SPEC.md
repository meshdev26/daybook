# Bookings — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec only; L7 implements in `src/web.rs`.
**Status:** v1 spec. WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L9 (data layer), L10 (security review), L6-C (token contract).
**Related:** `ARCHITECTURE-V1.4.md` §2 (D1–D5, D8 — binding contracts), `assets/app.css`, `src/web.rs` (`orders_page` scope/filter/table idiom, `subs_page` banner idiom, `keys_body` confirm idiom), `schema.sql` (`services`/`slots`/`bookings`), `docs/COMMANDS.md` (L4 updates the command list).

---

## 0. Summary of recommendations

1. **Dedicated page.** Route `GET /bookings` (session required) with three POST actions — `/bookings/confirm`, `/bookings/decline`, `/bookings/cancel` — exactly as contracted in `ARCHITECTURE-V1.4.md` §2. POST-redirect-flash; no client state.
2. **Scoping mirrors `orders_page`.** Sellers see their own bookings (scope from `session.seller_id`, never a query value); admin/owner see all. The gate is server-side, not UI-only.
3. **Actions follow the D3 state machine.** `held` → Confirm/Decline (the seller-response pair); `expired` → Confirm (resurrection, may lose the slot race) / Decline; `confirmed` → Cancel. Terminal rows (`declined`, `cancelled`) carry no actions. Decline and Cancel are destructive and use the existing confirm-first idiom (`confirm()`); Confirm is not destructive and must stay friction-free — it is the time-critical action.
4. **Read-only services panel** below the table (D8): name, price, duration, available-slot count, with pointers to `/serviceadd` and `/slotadd`. Services CRUD stays bot-only in v1.4.
5. **All bot copy lives in one place (§4)** — the exact strings for `/book` (fixed to say *requested*), `/mybookings`, `/cancelbooking`, `/bookingconfirm`, `/bookingdecline`, the sweeper DM, and the seller DMs — so L8, L4 and this page speak one vocabulary.
6. **No new tokens.** The page is buildable entirely from existing primitives (`panel`, `stat`, `table-wrap`, `badge`, `btn*`, `inline-form`, `empty`, `mono`, `help`, `sub`) — see §11.

---

## 1. Page route & navigation

### Routes (verbatim from `ARCHITECTURE-V1.4.md` §2)

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/bookings` | Render table, filter, count cards, services panel | session required; seller → scope = `session.seller_id`; admin/owner → unscoped (`orders_page` pattern, `web.rs:4366-4370`) |
| POST | `/bookings/confirm` | Set one booking to `confirmed` | session required; seller own / admin any; `log_action` audit |
| POST | `/bookings/decline` | Set one booking to `declined`, free the slot | session required; seller own / admin any; `log_action` audit |
| POST | `/bookings/cancel` | Set one booking to `cancelled`, free the slot | session required; seller own / admin any; `log_action` audit |

All paths are subject to the existing base-path prefix (`WebState::url`) and the 30s request timeout. Unauthenticated requests redirect to `/login` (303), exactly like `orders_page` / `subs_page`.

### Navigation placement

Add a **"Bookings"** entry to the `items` vec in `rail_nav` (`src/web.rs:461-473`), **after "Subscriptions", before "Analytics"**:

`Dashboard · Channels · Plans · Products · Codes · Orders · Finance · Subscriptions · Bookings · Analytics · Posts · Broadcast`

Rationale:
- Bookings are a buyer-activity surface like Orders and Subscriptions, so they belong in that run of the rail, not in the owner-only Manage group and not in the Settings footer.
- Placing it after Subscriptions keeps the money pair (Orders → Finance) intact and puts the two non-money fulfilment surfaces (Subscriptions, Bookings) side by side.
- One entry keeps the rail scannable on mobile (the shell collapses at ≤900px). Markup change only — no CSS needed; entries are already styled.

Note: `SUBS_UX_SPEC.md` §1 placed Subscriptions "between Products and Posts"; the live `rail_nav` has since grown (Codes, Orders, Finance, Analytics, Broadcast). This spec anchors to the live `rail_nav`, not the older prose.

### Why not fold into `/orders`

Orders carry shipment/tracking semantics; bookings carry a time-boxed response window (D3) with different actions, badges and urgency. Mixing them would force a two-vocabulary table on one page. A dedicated page also gives sellers a stable URL for "check your bookings" support links.

---

## 2. Information architecture / layout

Single column, `max-width: var(--measure)`, in the standard console shell (`html_page`). Top-to-bottom order:

1. **Page header** — `h1 Bookings` + `.sub` "service appointments and responses" + no header action slot (the page's primary action is per-row; there is no page-level mutation).
2. **Status banner slot** — action outcomes via PRG redirect params (§8).
3. **Count cards row** — four `.stat` cards: Held, Confirmed, Expired, Total (§3.2).
4. **Filter bar** — status chips (§3.1).
5. **Bookings table** (or empty state) — §3.3.
6. **Services panel** (read-only) — §3.6.
7. **Footer help** — scoping note ("Showing your bookings" vs "Showing all sellers' bookings") and the 200-row note (§10).

On mobile the count cards stack (`.grid-2` collapses at ≤1100px), the chips wrap, and the table scrolls horizontally inside `.table-wrap`. No other horizontal page scroll.

---

## 3. Table & filter spec

### 3.1 Status filter

Values (exact, case-sensitive, in this order): `all`, `held`, `confirmed`, `expired`, `declined`, `cancelled`. Default `all`.

- Rendered exactly like the Orders filter (`orders_page:4441-4465`): a `<nav aria-label="Filter bookings by status" class="inline-form">` of link chips pointing at `/bookings?status={value}` (base-prefixed). The active value carries `aria-current="page"` and `btn btn-secondary btn-sm`; inactive values are `btn btn-ghost btn-sm`.
- Unknown or missing `status` is **normalized to `all`** silently (no error) — shared links and typed URLs stay forgiving.
- Filter state lives in the URL only (shareable, survives refresh); never in a cookie or session.
- The filter does not interact with the banner params; both may coexist (`/bookings?status=held&done=confirm&b=…`).

### 3.2 Count cards

Four cards, in this order: **Held**, **Confirmed**, **Expired**, **Total**.

- Built from the accepted `.stat` primitive (`assets/app.css:338-347`): each card is a `.panel` containing `<div class="stat"><span class="stat-value">{n}</span><span class="stat-label">held</span></div>`. Label and number are both present (no number-only cards).
- Counts are derived server-side from the scoped booking list (all statuses, before the table's 200-row cap — counts must not be wrong when the cap clips the table; see §10.5).
- `Total` is the scoped row count, not the sum of the three status cards, so a future status is still counted.
- **Why not all five statuses:** Declined and Cancelled are terminal and low-value for triage; the page's job is the response window. The filter chips still expose every status. Four cards also collapse to a 2×2 grid instead of an awkward 5-card stack at ≤1100px.
- Held is the actionable number — it is the seller's to-do count inside the response window.

### 3.3 Bookings table

Columns, in this order:

| # | Header | Source | Notes |
|---|---|---|---|
| 1 | Booking | `booking_id` | mono; the 8-char token (`gen_token(8)`), not the integer row id — it is the id every bot string and audit row uses |
| 2 | Seller | `seller_id` | **admin/owner view only**; omitted entirely for sellers (not an empty column) — `SUBS_UX_SPEC` §3.3 precedent |
| 3 | Buyer | `user_id` | mono; full value (operator needs it for support; masking is an export-time rule per `SUBS_UX_SPEC` §5.5, not a v1 HTML rule) |
| 4 | Service | service name via `service_id` JOIN | `esc()`'d free text |
| 5 | Slot | `slots.starts_at` JOIN | mono; stored value rendered verbatim (RFC3339 after D4 validation; add `title` with a long form if L7 formats it). `—` when `slot_id` is NULL (legacy rows) |
| 6 | Status | `status` | badge per §3.4; text is the status word, never color alone |
| 7 | Expires | `expires_at` | mono; the seller-response deadline (D3). `—` when NULL. For non-held rows it is history, not urgency |
| 8 | Actions | — | per-status controls (§3.5) |

- Row order: newest first (`id DESC`), matching `list_bookings`.
- Wrapped in `.table-wrap`; `<table aria-label="Bookings">`; `<thead>`/`<tbody>` with `scope="col"` on every header and a `<caption class="sr-only">Bookings with status, slot time and response deadline</caption>`.
- Every dynamic cell passes through `esc()`; `booking_id`, `seller_id`, `user_id` are tokens/integers formatted directly.

### 3.4 Status vocabulary & badge mapping

Statuses are the DB values, lowercase, shown as the badge word. Mapping (all existing classes; `order_badge`/`subs_body` precedents):

| Status | Class | Reading |
|---|---|---|
| `held` | `badge badge-warning` | awaiting the seller's response, inside the window — the "pending" precedent |
| `confirmed` | `badge badge-success` | fulfilled — the "delivered/active" precedent |
| `expired` | `badge badge-danger` | response window missed; recoverable only while the slot is free — same word maps to `badge-danger` on Subscriptions |
| `declined` | `badge badge-danger` | closed without a fulfilled booking (seller refused) |
| `cancelled` | `badge` (neutral) | mutually-agreed close (buyer or seller called it off); not an alarm |

The badge word is always present, so color is never the only channel. The class is chosen from this allow-list, never from raw input.

### 3.5 Per-row actions (per status × role)

Every action is a `<form method="post" class="inline-form">` with a hidden `booking_id`, posting to the base-prefixed route. The server re-derives the role and scope from the session — the buttons are affordances, not the gate.

| Row status | Buttons | Route | Confirm-first? |
|---|---|---|---|
| `held` | **Confirm** (`btn btn-sm`) · **Decline** (`btn btn-danger btn-sm`) | `/bookings/confirm` · `/bookings/decline` | Confirm: **no** (time-critical, non-destructive). Decline: **yes** |
| `expired` | **Confirm** (`btn btn-sm`) · **Decline** (`btn btn-danger btn-sm`) | `/bookings/confirm` · `/bookings/decline` | Confirm: no — but failure is legitimate (slot race, §8) and must render as a banner, not dead UI. Decline: yes |
| `confirmed` | **Cancel** (`btn btn-danger btn-sm`) | `/bookings/cancel` | **yes** |
| `declined`, `cancelled` | none (empty cell) | — | — |

Rules and rationale:

- **Confirm on `expired` is D3 resurrection.** It succeeds only while the slot is `available`; the loser of the race gets the `error=slot` banner (§8). The button stays rendered on every `expired` row — the correct affordance for "still want this?" — with the outcome decided server-side.
- **No Cancel on `held` rows.** Decline is the semantically right seller response to a held request (buyer is told the request was refused); offering Cancel beside it would put two near-identical slot-freeing buttons in one cell. The server still accepts cancel on `held` (the state machine's `held → cancelled` remains legal for `cancel_booking`-class paths); only the presented affordance is scoped down.
- **Decline is legal on `held` and `expired` only; Cancel on `confirmed`** — exactly the transitions in the `ARCHITECTURE-V1.4.md` §2 state machine. Terminal rows render no buttons; the server rejects any transition out of a terminal state.
- **Confirm-first idiom (destructive):** Decline and Cancel use the existing native pattern (`keys_body:6828`): `onclick="return confirm('…')"` with the booking id in the message — "Decline booking {booking_id}? The buyer is notified and the slot is freed." / "Cancel booking {booking_id}? The buyer is notified and the slot is freed." No silent submit; `confirm()` is keyboard- and screen-reader-accessible, and the follow-up redirect returns focus to the top of the page (acceptable for a full navigation).
- **Accessible names carry the row id:** `aria-label="Confirm booking {booking_id}"`, `"Decline booking {booking_id}"`, `"Cancel booking {booking_id}"` — never bare "Confirm"/"Cancel" (virtual-buffer users must know which row they act on). Mirrors `orders_page`'s per-row `aria-label` pattern.
- **Touch targets ≥ 44×44px on mobile** (§5.7); if `.btn-sm` falls below that at ≤480px, use the standard `.btn` size inside the actions cell on small screens rather than changing the global class.

### 3.6 Services panel (read-only, D8)

Below the table, an `<article class="panel">` — `h3 Services` + one-line help + a table:

- Help line: "Services are managed in the bot: /serviceadd to add a service, /slotadd to add slots. Buyers see /services."
- Columns: **Service** (name, esc'd) · **Price** (`services.price`, display-only in v1.4 — D2) · **Duration** (`{duration_minutes} min`) · **Available slots** (count of that service's slots with `status='available'` and `starts_at` in the future; `0` renders as `0`, never hidden) · **Seller** (admin/owner view only).
- Row order: newest first, matching `list_services`. Active-flagged but fully-booked services still render — the 0 tells the seller to run `/slotadd`.
- Empty state (no services in scope): `<p class="empty"><b>No services yet.</b> Add one with /serviceadd in the bot, then add slots with /slotadd.</p>`
- No edit/delete/add affordances anywhere in the panel — services CRUD is bot-only in v1.4 (D8 non-goal). The panel links nowhere; the help line is the pointer.

### 3.7 Seller vs admin/owner view

- **Seller:** rows scoped to `session.seller_id`; no Seller column; no cross-seller anything. Footer: "Showing your bookings only."
- **Admin/owner:** unscoped; Seller column present; footer: "Showing all sellers' bookings."
- Scope comes from the session role (`is_admin_role`), never from a query or form value.

---

## 4. Bot copy (exact strings)

Every buyer/seller-facing string for the v1.4 booking loop, in one place. `{placeholders}` are values; L8 sends them verbatim (plain text, no Markdown forced). L4 mirrors the command summaries in `docs/COMMANDS.md`.

### 4.1 `/book <service_name>` (fixes the "confirmed" lie — D3/D8)

- Usage (unchanged): `Usage: /book <service_name>`
- Success (hold claimed):
  ```
  Booking {booking_id} requested — {service_name} at {slot_starts_at}.

  The seller has up to 60 minutes to respond. You'll get a message when they confirm or decline.
  Cancel anytime: /cancelbooking {booking_id}
  ```
  Must say **requested**, never "confirmed" (today's text at `commands.rs:1730` lies). "up to 60 minutes" stays true when the slot starts sooner (the hold is `min(now + 60 min, slot start)`).
- Claim-race loser (Db `Err("slot no longer available")`): `Slot no longer available.`
- No free slot (unchanged): `No available slots.`
- Unknown service (unchanged): `Service not found.`

### 4.2 `/slotadd service | datetime [| capacity]` (D1 clamp + D4 validation)

- Usage (unchanged): `Usage: /slotadd service | datetime [| capacity]`
- Success, capacity ≤ 1 (unchanged): `Slot #{slot_id} created for {service_name} at {datetime}.`
- Success, capacity > 1 supplied (clamp notice, per D1, appended after the success line):
  `Capacity > 1 is reserved for class bookings (v1.5); slot created with capacity 1.`
- Rejected timestamp (past or unparseable — D4): `Could not create the slot. Use a future UTC time, format YYYY-MM-DD HH:MM.`

### 4.3 `/mybookings` (new, User)

- Empty state:
  `You have no bookings. Browse /services and book with /book <service_name>.`
- List (header + one block per booking, newest first):
  ```
  Your bookings (id | service | time | status):
  {booking_id} | {service_name} | {slot_starts_at} | {status}
  ```
  For `held` rows, append: `The seller has until {expires_at} to respond.`
  For `confirmed` rows, append: `Seller: {seller_display}.`
  Footer, when any row is `held` or `confirmed`: `Cancel: /cancelbooking <booking_id>`
- `seller_display` is the existing helper (`commands.rs:2486`) for the seller column.

### 4.4 `/cancelbooking <booking_id>` (new, User)

- Usage: `Usage: /cancelbooking <booking_id>`
- Success: `Booking {booking_id} cancelled. The seller has been notified and the slot is free again.`
- Seller DM (after `cancel_booking` returns the seller_id):
  `Booking {booking_id} ({service_name}) was cancelled by the buyer. The slot is free again.`
- Error (not found, not yours, or terminal — one message, no oracle):
  `Booking not found or already closed.`

### 4.5 `/bookingconfirm <booking_id>` (new, Seller; Admin/Owner: any seller's rows)

- Usage: `Usage: /bookingconfirm <booking_id>`
- Success: `Booking {booking_id} confirmed. The buyer has been notified.`
- Buyer DM:
  ```
  Booking {booking_id} confirmed — {service_name} at {slot_starts_at}.
  Seller: {seller_display}
  Change of plans? /cancelbooking {booking_id}
  ```
- Slot race lost (Db `Err("slot already taken")`, resurrection path):
  `Could not confirm {booking_id}: the slot is already taken.`
- Terminal state (Db errors on transitions out of `confirmed`/`declined`/`cancelled`):
  `Booking {booking_id} is already {status}.`
- Not found / not yours (one message, no oracle for sellers probing foreign ids):
  `Booking not found or already closed.`

### 4.6 `/bookingdecline <booking_id>` (new, Seller; Admin/Owner: any)

- Usage: `Usage: /bookingdecline <booking_id>`
- Success: `Booking {booking_id} declined. The buyer has been notified.`
- Buyer DM: `Booking {booking_id} ({service_name}) was declined by the seller. Find another time via /services.`
- Terminal / not found / not yours: same strings as §4.5.

### 4.7 `/bookings` (existing Seller+ view — one addition)

- Sellers/admins keep the current list view. When a **User** (buyer role) invokes it: `Bookings are a seller view. See your own bookings with /mybookings.`

### 4.8 Sweeper DM (D5 — buyer whose hold expired)

```
Booking {booking_id} ({service_name}) expired — the seller didn't respond in time, so the slot was released.
You can book again: /book {service_name}
```

- Sent by the `expire_held_bookings` pass for every row it transitions (the status change is the exactly-once guard). Send failure is logged at warn, never propagated (`main.rs:702-707` pattern).
- **Sellers are never DM'd on expiry** (noise control, per the architecture doc); buyers always are.

---

## 5. Accessibility (WCAG 2.1 AA — release floor)

1. **Semantics.** One `<h1>` per page (`Bookings`). Count cards live in a `<section aria-labelledby="bookings-counts-heading">` with an `<h2>` (styled as a section label); the services panel is an `<article>` with its own `h3`. No skipped heading levels.
2. **Filter as navigation.** Link chips are real `<a>` elements in a `<nav aria-label>`; the active one has `aria-current="page"`. No JavaScript required to filter.
3. **Status is never color-only.** Badges carry the status word (the leading dot is decorative). Count cards pair the number with a text label.
4. **Table semantics.** `<table aria-label="Bookings">`, `scope="col"` on every `<th>`, sr-only `<caption>` (§3.3).
5. **Action buttons.** Every button's accessible name includes the booking id (§3.5). Destructive actions confirm via native `confirm()` — keyboard and screen-reader accessible; no custom modal in v1.
6. **Alerts.** Success banners use `role="status" aria-live="polite"`; error banners use `role="alert" aria-live="polite"` — the exact markup vocabulary of `subs_page:3291-3357`. On a banner-bearing render, focus lands on the banner (L7: `tabindex="-1"` on the banner container, focused on load when present).
7. **Touch targets.** Row-action buttons ≥ 44×44px on touch devices (§3.5); chips keep ≥ 44px tap height when wrapped.
8. **Focus visibility.** Global `:focus-visible` ring (2px brand outline); no `outline: none` on any new element. No positive `tabindex`.
9. **Contrast.** Existing tokens only (verified ≥ 4.5:1 in `app.css`); badge/alert pairings are the shipped ones. No new color values.
10. **Reduced motion.** Handled globally (`app.css:492`); no new animation on this page. No auto-refresh or polling — the page is static between navigations.
11. **Empty states are real DOM text** (§8), never CSS `::before`.

---

## 6. Security & privacy

1. **Authentication.** Every route requires a valid session; unauthenticated → 303 `/login`.
2. **Authorization (server-side).** Scope derived from the session (seller → own; admin/owner → any), enforced inside the Db methods (`seller_scope`), not the handler. A seller POSTing another seller's `booking_id` must not mutate: respond with the `error=forbidden` flash (or 403 — L7 picks one and applies it to all three POSTs), mirroring `SUBS_UX_SPEC` §5.2.
3. **Input validation.** `booking_id` is allow-listed to the scoped row set by the Db call itself; a missing/invalid id redirects with `error=invalid` and touches nothing.
4. **No PII in URLs.** The only params are `status` and the booking token (an 8-char random token — a capability reference, not an identity).
5. **No PII in logs.** Trace/audit lines reference `booking_id` only, never `user_id`.
6. **Audit.** Each POST calls `log_action(account_id, &session.role, "booking_confirm"|"booking_decline"|"booking_cancel", "booking", booking_id, None)` per the route contract.
7. **Escaping.** All dynamic strings via `esc()`; badge classes from the §3.4 allow-list.
8. **CSRF.** Existing session-cookie posture (SameSite=Strict, HttpOnly); no new token scheme in v1.

---

## 7. Responsive behavior

| Viewport | Behavior |
|---|---|
| ≥1100px (desktop) | 2×2 count-card grid; table full measure width; services panel full width. |
| ≤1100px (tablet) | Cards stack to one column; chips wrap; rail still vertical. |
| ≤900px (mobile) | Rail becomes a horizontal wrapped bar; main padding reduces; table scrolls horizontally inside `.table-wrap` (the only horizontal scroll on the page). |
| ≤480px (small phone) | Row-action buttons grow to ≥44px targets and may stack within the Actions cell; the Booking id column stays visible first if feasible (sticky first column is a nice-to-have, not required). |

---

## 8. Banner keys, empty & error states

### 8.1 PRG banner keys (query params on GET `/bookings`)

Built exactly like `subs_page:3287-3363` (banners collect into a list; all pre-escaped HTML).

| Param | Banner | Class / role |
|---|---|---|
| `done=confirm&b={id}` | `Booking {id} confirmed.` | `alert-success` / `role="status"` |
| `done=decline&b={id}` | `Booking {id} declined.` | `alert-success` / `role="status"` |
| `done=cancel&b={id}` | `Booking {id} cancelled. The slot is free again.` | `alert-success` / `role="status"` |
| `error=invalid` | `Invalid booking id.` | `alert-error` / `role="alert"` |
| `error=notfound` | `Booking not found or already closed.` | `alert-error` / `role="alert"` |
| `error=forbidden` | `Booking actions are limited to your own bookings.` | `alert-error` / `role="alert"` |
| `error=slot&b={id}` | `Could not confirm booking {id}: the slot is no longer available.` | `alert-error` / `role="alert"` |
| `error=state&b={id}` | `Booking {id} can no longer be changed.` | `alert-error` / `role="alert"` |

Banner copy claims only what the console did — it never promises a DM was sent (the arch contracts DMs on the bot paths; console-side DMs are an L8 decision, §10.3).

### 8.2 Empty states

1. **No bookings at all (any scope, filter `all`):** `<p class="empty"><b>No bookings yet.</b> Buyers book a slot with /book <service_name> in the bot — add slots with /slotadd.</p>`
2. **Filter returns no rows but others exist:** `No {status} bookings.` plus a link to clear the filter (`/bookings`). Short; not an error.
3. **Seller with no rows:** same as (1); footer notes "Showing your bookings only."

### 8.3 Error states

- All POST failures are flashes per §8.1 — no DB write on any failure path (Db-layer scope checks reject before mutating).
- Unknown `status` is normalized, not an error (§3.1).
- Unauthenticated: 303 to `/login` (no body).
- Data-load failure (DB error): render the page shell with an `alert-error` "Could not load bookings. Try refreshing." and omit the table; never crash the page.

---

## 9. Out of scope (v1.4, per the architecture doc)

- Priced bookings / Crypto Pay invoice per booking (v1.5, pending the L5 ruling Q1) — `services.price` stays display-only in the services panel.
- Class capacity (N seats) — D1 defers to v1.5.
- Console services CRUD — read-only panel only (D8); `/serviceadd`/`/slotadd` remain bot-only.
- Buyer console — buyers act via the bot (`/mybookings`, `/cancelbooking`); the console is a seller/admin surface.
- Editing slots, rescheduling, or moving a booking between slots.
- Booking search by `user_id`/`service`; sorting beyond newest-first; bulk actions.
- Pagination beyond the fixed 200-row cap + footer note (§10.5).
- UTC conversion / localization of times — stored values render verbatim (non-goal in the architecture doc).
- DM delivery status in banners (§8.1).

---

## 10. Backend contracts L7/L8/L9 must satisfy (flags, not fixes)

These are required for the spec to be implementable; flagged so the owning rungs act. Deviation from `ARCHITECTURE-V1.4.md` signatures is an L3 defect — so are gaps in it.

1. **Console list method is missing from the contract (gap).** `list_bookings(seller_scope)` has no slot time and no service name; `list_bookings_for_user(user_id)` is buyer-scoped. The D8 table needs both. Required: a console list, recommended signature
   `list_bookings_filtered(seller_scope: Option<i64>, status_opt: Option<&str>, limit: i64) -> Vec<BookingConsoleRow>` (booking_id, seller_id, user_id, service_name, status, slot_starts_at, expires_at, created_at; JOIN services + slots). SQL-level `WHERE status = ?` is required — filter-after-LIMIT silently hides matching rows (same rationale as `SUBS_UX_SPEC` §9). **Owner: L9.**
2. **Console cancel has no Db method (contract mismatch).** The route table says `POST /bookings/cancel … seller own / admin any`, but the Db contract only lists buyer-scoped `cancel_booking(user_id, booking_id)`. The console needs a seller/admin-scoped cancel accepting `held|confirmed` (state machine: `confirmed → cancelled` via console; `held → cancelled` stays legal). Recommend `cancel_booking_scoped(seller_scope: Option<i64>, booking_id: &str)`. **Owner: L9 + L8.**
3. **Console-side DMs are unspecified.** The arch contracts DMs on the bot paths (`confirm_booking` returns `(seller_id, buyer_user_id)` *for DMs*). Recommendation: the three POST handlers best-effort DM the counterpart using the §4 strings (buyer on confirm/decline/cancel; nobody on expiry). If skipped, §8.1 copy stays truthful as written. **Owner: L8.**
4. **Available-slot counts for the services panel.** No aggregate exists today (`list_slots` is per service). Recommend `available_slot_counts(seller_scope) -> Vec<(service_id, i64)>` (slots `status='available'` and future), or compute per service in the handler for v1.4 volumes. **Owner: L9.**
5. **Row cap.** Fetch limit 200 with footer note "Showing the first 200 bookings." when more exist — never silently truncate. Counts (§3.2) derive from the unscoped-by-limit query. **Owner: L7.**
6. **`log_action` detail.** Detail is the `booking_id` token only. **Owner: L8/L7.**

---

## 11. Token-contract flags (L6-C)

**None.** The page is buildable from existing primitives only: `panel`, `stat`/`stat-value`/`stat-label` (already accepted and shipped, `app.css:338-347`), `table-wrap`, `badge` (+ the three variants), `btn`/`btn-secondary`/`btn-danger`/`btn-ghost`/`btn-sm`, `inline-form`, `empty`, `mono`, `help`, `sub`, `grid-2`, `alert*`. The rail entry is a markup change in `web.rs`, no CSS. Confirm-first reuses the native `confirm()` precedent from `keys_body`. No new classes, no inline styles, no new color tokens.

---

## 12. Acceptance checklist (for L7 / L3 verification)

- [ ] `GET /bookings` renders for seller (own rows) and admin/owner (all rows); unauthenticated redirects to `/login`.
- [ ] Nav contains "Bookings" between Subscriptions and Analytics.
- [ ] `?status=` filters rows; unknown values behave as `all`; counts are unaffected by the filter or the row cap.
- [ ] Four count cards (Held, Confirmed, Expired, Total) show correct scoped counts.
- [ ] Owner/admin see the Seller column; sellers do not (column omitted, not empty).
- [ ] Table columns: Booking, Buyer, Service, Slot, Status, Expires, Actions (+Seller for admin); all `esc()`'d; `scope="col"` on headers; sr-only caption.
- [ ] `held` rows: Confirm + Decline; `expired` rows: Confirm + Decline; `confirmed` rows: Cancel; terminal rows: no buttons.
- [ ] Decline and Cancel require `confirm()`; Confirm does not. All action buttons have aria-labels carrying the booking id.
- [ ] Seller POSTing another seller's booking does not mutate and gets the forbidden/notfound response.
- [ ] Confirm on an `expired` booking with a taken slot flashes `error=slot` and mutates nothing.
- [ ] All three POSTs write a `log_action` audit row with the booking_id.
- [ ] Services panel renders name, price, duration, available-slot count (0 shown as 0) with no CRUD affordances; empty state present.
- [ ] Banners match §8.1 exactly (wording, class, role); alerts announced via role/aria-live.
- [ ] Empty and filter-empty states render with guidance.
- [ ] No `user_id` in URLs or logs; booking token is the only row reference in a URL.
- [ ] Keyboard-operable throughout, visible focus, no page-level horizontal scroll at 375/768/1024/1440 (table scroll only), 44px touch targets on mobile.
- [ ] No inline styles or undocumented CSS classes; no new color values.

---

## 13. Open questions for L6-C

1. **Token contract: none.** §11 stands — no new tokens are needed; the page reuses the accepted `.stat` primitive and the existing badge/button/panel vocabulary.
2. **Non-token, for the record (no response needed unless overridden):** held rows present Decline rather than Cancel (§3.5). If L6-C wants both affordances on `held`, the server already permits it — it is a one-markup change, but the recommended default is the single verb.

---

*End of spec.*
