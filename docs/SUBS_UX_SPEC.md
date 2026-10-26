# Subscriptions — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec only; L7 implements in `src/web.rs`.
**Status:** v1 spec. WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L9 (data layer), L10 (security review), L6-C (token contract).
**Related:** `DESIGN.md`, `assets/app.css`, `src/web.rs` (channels/settings/ai pages), `src/db.rs` (subscription methods), `src/api.rs` (`/api/v1/subscriptions`), MCP `subs_report`.

---

## 0. Summary of recommendations

1. **Dedicated page.** Route `GET /subs` (session required), with `POST /subs/revoke` and `POST /subs/expire-check`. Rationale in §1.
2. **Status filter + count cards on one page.** Filter is a query param (`?status=`); count cards are derived server-side. No client-side state.
3. **Owner-only mutations.** Sellers are scoped to their own rows and have no revoke / expire-check affordance. The gate is server-side, not UI-only.
4. **Full `user_id` in the console.** Operator needs it; any future export must mask it (see §5). No masking in v1 HTML.
5. **Reuse existing DB methods** plus one recommended filtered list method (§9). No schema changes.
6. **No pagination, extend, CSV, or bulk actions in v1** (§8). Fixed fetch limit of 200 with an explicit "showing first 200" note.

---

## 1. Page route & navigation

### Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/subs` | Render list, filter, count cards | session required; seller-scoped rows |
| POST | `/subs/revoke` | Set one subscription to `revoked` | **owner only** |
| POST | `/subs/expire-check` | Mark all currently-expired active rows `expired` | **owner only** |

All paths are subject to the existing base-path prefix (`WebState::url`) and the 30s request timeout. Unauthenticated requests redirect to `/login` (303), exactly like `channels_page` / `products_page`.

### Navigation placement

Add a **"Subscriptions"** entry to `rail_nav` in the **Operate** group, between **Products** and **Posts**:

`Dashboard · Channels · AI · Plans · Products · Subscriptions · Posts`

Rationale:
- Subscriptions are a read/monitor surface over plans and products, so they follow the catalog entries rather than precede them.
- They are operational (like Channels/Posts), not deployment configuration, so they stay out of the Settings footer.
- One entry keeps the rail scannable on mobile (it already collapses at ≤640px).

### Why not fold into `/dashboard`

- The dashboard is the first-sale funnel; a subscription table would push the action forms below the fold and hurt time-to-first-sale.
- Subscriptions have their own filter, counts, and destructive action, which warrant a full measure-width page and a stable URL for support links.

---

## 2. Information architecture / layout

Single column, `max-width: var(--measure)`, in the standard console shell (`html_page`). Top-to-bottom order:

1. **Page header** — `h1 Subscriptions` + `.sub` "buyer access and expiry" + right-aligned primary-ish affordance slot holding the **Run expire check** button (owner only) and the status filter.
2. **Status banner slot** — save/action outcomes (success/error alerts).
3. **Count cards row** — five cards: Active, Expired, Revoked, Refunded, Total (§3.2).
4. **Filter bar** — status chips (§3.1).
5. **Subscription table** (or empty state) — §3.3.
6. **Footer help** — scoping note ("Showing your subscriptions" vs "Showing all sellers' subscriptions") and the 200-row note.

On mobile the count cards stack, the filter chips wrap, and the table scrolls horizontally inside `.table-wrap`. No other horizontal page scroll.

---

## 3. Field / table spec

### 3.1 Status filter

Values (exact, case-sensitive, in this order): `all`, `active`, `expired`, `revoked`, `refunded`. Default `all`.

- Rendered as a `<nav aria-label="Filter subscriptions by status">` of link chips, one per value, each pointing at `/subs?status={value}` (base-prefixed). The active value carries `aria-current="page"` and a selected visual style.
- Unknown or missing `status` is **normalized to `all`** silently (no error). This keeps shared links and typed URLs forgiving.
- Filter state is reflected in the URL so it is shareable and survives refresh; it is never stored in a cookie or in session state.
- The filter does not reset the success banner's `expired` query param; both may coexist (`/subs?status=expired&expired=5`).

### 3.2 Status count cards

Five cards, in this order: **Active**, **Expired**, **Revoked**, **Refunded**, **Total**.

- Data: `subscription_status_counts(scope)` gives only statuses that exist; the UI must default any missing status to `0`. `Total` comes from `count_subscriptions(scope, None)` (not the sum of the four, so any future status is still counted).
- Each card shows the status word (label) and the count in the data font. Label and number must both be present (no number-only cards).
- Card order and labels are fixed so muscle memory and tests are stable.

### 3.3 Subscription table

Columns, in this order:

| # | Header | Source | Notes |
|---|---|---|---|
| 1 | ID | `id` | mono, right-aligned numeric |
| 2 | Seller | `seller_id` | **owner view only**; omitted entirely for sellers (not an empty column) |
| 3 | User | `user_id` | mono; full value (see §5) |
| 4 | Channel | `channel_id` | mono |
| 5 | Status | `status` | badge (`badge-success` active, `badge-danger` revoked/expired, neutral otherwise); text is the status word, never color alone |
| 6 | Expires | `expires_at` | mono; stored RFC3339 value rendered verbatim |
| 7 | Actions | — | **owner view only**; revoke control per row |

- Row order: `id DESC` (newest first), matching `list_subscriptions`.
- The table is wrapped in `.table-wrap` so it scrolls horizontally on small screens; the header row stays visible (`thead` sticky is optional, not required).
- A visually hidden or `aria-label="Subscriptions"` on the `<table>` so screen readers announce it; every `<th>` has `scope="col"`.

### 3.4 Revoke control (owner only)

Per row, inside the Actions cell:

- A single destructive button labelled **Revoke**, `btn btn-danger btn-sm`, posting to `/subs/revoke` with a hidden `id` field.
- Because it is destructive, it must require confirmation before submit. Use a native `confirm()` in the button's `onclick` (or an equivalent accessible confirmation) — no silent submit. Example intent: "Revoke subscription #123? The buyer loses access; this cannot be undone from the console."
- The button's accessible name must include the row id (e.g. `aria-label="Revoke subscription 123"`), not just "Revoke", so screen-reader users acting on a virtual buffer know which row.
- Minimum touch target 44×44px on mobile; if `.btn-sm` falls below that, use the standard `.btn` size inside the actions cell on small screens (flagged in §10).

### 3.5 Expire-check control (owner only)

- One button, **Run expire check**, `btn btn-secondary btn-sm` (secondary because it is not the page's primary destructive action; revoke is per-row), posting to `/subs/expire-check`.
- Placement: page header action slot, visible only to owners. Sellers do not see it.
- Feedback: on success redirect to `/subs?expired={n}` (preserving the current `status` filter if present) and render an `alert-success`: "Marked {n} subscription(s) expired." If `n == 0`, still show "No subscriptions were due for expiry." so the action never appears to do nothing.
- On failure (should not normally happen) render `alert-error` in place, matching `channels_error` / `settings_error`.

### 3.6 Seller view

- Sees only their own rows (scope from session, never from a query/form value).
- No Seller column, no Actions column, no expire-check button.
- Count cards reflect the seller's own counts.
- Filter still works.
- Footer help reads "Showing your subscriptions only."

---

## 4. Accessibility (WCAG 2.1 AA — release floor)

1. **Semantic structure.** One `<h1>` per page. Count cards live in a `<section aria-labelledby="subs-counts-heading">` with an `<h2>` (visually styled as a section label). Table uses `<thead>`/`<tbody>`, `scope="col"` on every header, and an `aria-label` on the table.
2. **Filter as navigation, not a form.** Link chips are real `<a>` elements in a `<nav>` with `aria-label`; the active one has `aria-current="page"`. Keyboard users tab through them; Enter activates. No JavaScript required to filter.
3. **Status is never color-only.** Badges contain the status word plus a leading dot; the dot is decorative (`aria-hidden` on the dot is acceptable because the word carries the meaning). Count cards pair number with a text label.
4. **Focus visibility.** Rely on the global `:focus-visible` ring (2px brand outline). Do not remove outlines on any new element.
5. **Destructive confirmation.** `confirm()` is keyboard and screen-reader accessible; the follow-up redirect returns focus to the top of the page (acceptable for a full navigation). No custom modal focus trap is required in v1; if a modal is used instead, it must trap focus and restore it on close.
6. **Alerts.** Outcome banners use `role="alert"` (error) / `role="status"` (success) and `aria-live="polite"` so assistive tech announces them. Existing alert markup should gain these attributes (flagged in §10 if not already present).
7. **Touch targets.** Revoke and expire-check buttons must be at least 44×44px on touch devices. If `.btn-sm` is smaller, override for these two controls on mobile rather than changing the global button.
8. **Reduced motion.** Already handled globally (`prefers-reduced-motion`). No new animation should be added for this page.
9. **Contrast.** All text uses existing tokens, which are pre-verified ≥4.5:1 on surface. Any new token (§10) must be verified before use.
10. **No auto-refresh / polling.** Avoids motion and surprise; the page is static between navigations.

---

## 5. Security & privacy

1. **Authentication.** Every route requires a valid session; unauthenticated → 303 `/login`.
2. **Authorization (server-side).**
   - List/count: seller scope = `Some(seller_id)` for sellers, `None` for owners. Derived from the session, never from a query/form value.
   - Revoke: owner only. A non-owner POST returns 403 (`forbidden_page`-equivalent for the POST, or redirect to `/subs` with `alert-error`; L7 picks, but the mutation must not occur).
   - Expire-check: owner only, for the same reason (it iterates all sellers' rows).
3. **Input validation.** `id` is parsed as `i64`; an invalid or missing value redirects to `/subs` without touching the DB. `status` is allow-listed to the five values; anything else becomes `all`.
4. **Scoping of mutations.** `set_subscription_status` is called only after the owner check. In v1 owners may revoke any row; if seller revoke is added later, it must first verify `seller_id` ownership.
5. **`user_id` handling.**
   - **v1 console:** full value shown. Operators need it for support and reconciliation.
   - **HTML escaping:** `user_id`, `channel_id`, `expires_at`, and `status` all pass through `esc()`. `id` and `seller_id` are integers and formatted directly.
   - **No PII in URLs.** The filter uses `status` only; revoke posts `id`. The `expired={n}` param contains a count, not an identity.
   - **No PII in logs.** Trace lines reference subscription `id` only, never `user_id` or `channel_id`.
   - **Export (out of scope, §8):** when CSV/JSON export is added, `user_id` must be masked (e.g. `***` + last 3 digits) by default, with an explicit opt-in to reveal full ids for owners. The MCP precedent (`subs_report`, `include_user_ids=false` default) sets this expectation.
6. **Secrets.** No token, key, or credential is rendered on this page. The page does not read `.env`.
7. **CSRF.** No new mechanism is added; the page relies on the existing session-cookie posture. If a CSRF token is introduced later, all forms here must carry it.
8. **No unsafe HTML.** All dynamic strings escaped; status badge class is chosen from the allow-list, never from raw input.

---

## 6. Responsive behavior

- **≤640px (existing breakpoint):** rail collapses to a horizontal bar; main padding reduces; `.form-row` stacks; table scrolls inside `.table-wrap`.
- **Count cards:** `.grid-2` collapses to one column at ≤1100px, so five cards stack on tablet/phone. Acceptable; if a denser layout is wanted it needs a new grid token (§10).
- **Filter chips:** wrap to multiple rows; each chip keeps ≥44px tap height.
- **Table:** horizontal scroll is the only horizontal scroll on the page; the first column (ID) should remain visible if feasible (sticky first column is a nice-to-have, not required).
- **Touch targets:** revoke/expire-check buttons meet 44×44px (§4.7).

---

## 7. Empty & error states

### Empty states

1. **No subscriptions at all (any filter, any scope):**
   "No subscriptions yet. They appear when a buyer pays or when you grant access with `/grant` in the bot." Use the `.empty` block with a `<b>` lead line.
2. **Filter returns no rows but others exist:**
   "No {status} subscriptions." plus a link to clear the filter (`/subs`). Keep it short; do not imply an error.
3. **Seller with no rows:** same as (1), with footer note "Showing your subscriptions only."

### Error states

1. **Unknown status param:** normalized to `all` (§3.1) — not an error.
2. **Invalid `id` on revoke:** redirect to `/subs` with `alert-error` "Invalid subscription id." No DB write.
3. **Revoke DB failure:** redirect to `/subs` with `alert-error` "Could not revoke subscription {id}." (id only, no PII).
4. **Non-owner mutation attempt:** 403 via the existing `forbidden_page` pattern for GET; for POST, redirect to `/subs` with `alert-error` "Subscriptions management is restricted to the console owner." and do not mutate.
5. **Expire-check failure:** in-place `alert-error` (§3.5).
6. **Unauthenticated:** 303 to `/login` (no body).

All alerts use the existing `.alert` / `.alert-error` / `.alert-success` classes and gain `role="alert"` / `role="status"` (§4.6).

---

## 8. Out of scope (v1)

- Extending or changing `expires_at` (extend days) — status-only mutations.
- CSV/JSON export of any kind (and therefore the export-time masking rule is specified but not implemented).
- Pagination / infinite scroll. Fixed limit of 200 rows; if more exist, show a footer note "Showing the first 200 subscriptions." Do not silently truncate.
- Sorting by columns other than newest-first.
- Search/filter by `user_id` or `channel_id`.
- Bulk revoke or bulk status change.
- Refunding (web only displays `refunded`; it is set by payment reconciliation elsewhere).
- Channel kick on web expire-check. The web marks rows `expired` only; channel removal remains the bot's `/expirecheck` + sweeper responsibility (`expire_subscriptions` in `src/lib.rs`). Unifying these is a future change.
- Seller-initiated revoke.
- Inline editing of any field.

---

## 9. Backend contracts

No new tables or migrations. Existing methods suffice for the page:

| Need | Method | Notes |
|---|---|---|
| List rows (scoped) | `list_subscriptions(seller_scope, limit)` | No status filter today |
| Count by status | `subscription_status_counts(seller_scope)` | Returns only present statuses |
| Total count | `count_subscriptions(seller_scope, None)` | |
| Revoke | `set_subscription_status(sub_id, "revoked")` | |
| Expire-check | `expired_subscriptions()` + `mark_subscription_expired(id)` | Global; owner-gated at the handler |

**Recommendation to L9 (correctness/perf):** add a filtered list method rather than filtering in Rust:

`list_subscriptions_filtered(seller_scope: Option<i64>, status_opt: Option<&str>, limit: i64) -> Vec<(i64, i64, i64, String, String, String)>`

Rationale: filtering after `LIMIT` (the only option with today's method) can hide matching rows whenever the newest N are not of the requested status, so the filter would be silently wrong. A SQL-level `WHERE status = ?` (when `status_opt` is `Some`) fixes this and keeps the page correct at any data volume. If L9 cannot add it before implementation, L7 must fetch with a much larger limit and filter in Rust, and the spec's 200-row note must become "first 200 of the requested status may be incomplete" — not acceptable for v1, so treat the filtered method as **required**.

**L8 handler notes:**
- Seller scope derived from `session.seller_id` for sellers, `None` for owners.
- Revoke handler parses `id`, checks owner, calls `set_subscription_status(id, "revoked")`. Optionally verify the row exists first to distinguish "not found" from "done" (nice-to-have; v1 may simply call and redirect).
- Expire-check handler iterates `expired_subscriptions()` and calls `mark_subscription_expired(id)` for each, returning the count. It does **not** call `expire_subscriptions` (no bot kick) per §8.

---

## 10. Token-contract flags (L6-C)

The following are **not** in today's `assets/app.css` and must be added there (and documented in `DESIGN.md`) before use. Do not inline-style them in `src/web.rs`.

1. **Count card numerals.** A `.stat-value` (and matching `.stat-label`) primitive for the large number in each count card. Reuse `.panel` for the card container; only the numeral size is new.
2. **Filter chips.** A `.filter-chips` container and `.filter-chip` element with a selected state keyed on `aria-current="page"` (e.g. `.filter-chip[aria-current="page"]`). Today's `.badge` is close but is a status dot, not a navigation affordance; do not overload it.
3. **Alert live regions.** If `.alert` does not already carry `role`/`aria-live` in markup, the implementation adds the attributes in HTML (no CSS change), but any styling of live-region focus must be tokenized.
4. **Touch-target override.** A mobile-only min-height for `.btn-sm` when it is used for destructive row actions, or a dedicated `.btn-touch` modifier. Prefer a modifier over changing global `.btn-sm`.
5. **Reduced motion.** Already present globally; no change. Any new transition added for chip selection must be covered by the existing `prefers-reduced-motion` block.
6. **Optional sticky table header / sticky first column.** If implemented, add as a documented utility, not an ad-hoc style.

Anything not listed above should be built from existing primitives (`panel`, `table-wrap`, `mono`, `num`, `badge`, `alert`, `empty`, `inline-form`, `btn*`).

---

## 11. Acceptance checklist (for L3 verification)

- [ ] `GET /subs` renders for owner and seller; unauthenticated redirects to `/login`.
- [ ] Nav contains "Subscriptions" between Products and Posts.
- [ ] `?status=` filters rows; unknown value behaves as `all`.
- [ ] Five count cards show correct counts for the scoped user, with missing statuses as 0.
- [ ] Owner sees Seller column, User, Channel, Status, Expires, Actions; seller sees no Seller/Actions column.
- [ ] Revoke (owner) sets status to `revoked` and row updates after redirect.
- [ ] Seller POST to revoke/expire-check does not mutate and returns the restricted response.
- [ ] Expire-check marks expired rows and reports the count.
- [ ] Empty and filter-empty states render with guidance.
- [ ] No `user_id` appears in URLs, logs, or error messages beyond the rendered cell.
- [ ] All interactive elements keyboard-operable with visible focus; alerts announced.
- [ ] Page usable at 360px width with table scroll, no page-level horizontal scroll.
- [ ] No inline styles or new undocumented CSS classes in the implementation.
