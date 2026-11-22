# EDIT_FLOW_SPEC — Console edit-flow audit

Truth: `DESIGN.md` + `assets/app.css`. MASTER.md deprecated. Territory: this
spec only. All file:line evidence verified 2026-11-01 on the working tree.

Scope: every console edit surface NOT already redesigned (setup wizard,
settings copy, bots cards, channels add/edit, and the VISUAL_POLISH wave are
prior work — verified still holding, see §1). Audited against the six
heuristics:

1. One primary action per view; secondary subordinate (`btn-secondary`/`ghost`),
   destructive isolated (`btn-danger` + consequence-specific confirm naming the
   object).
2. Edit-in-place for 1–3 small fields; progressive disclosure for complex ones;
   no wall-of-fields; no ambiguous Save.
3. Immediate visible feedback on every mutation (PRG banner or re-render),
   honest about scope ("Saved." vs "Saved — applies on restart.") and about
   nothing-changed.
4. Forms carry only fields a user must decide NOW; optional stuff collapsed.
5. Keyboard/AA floor: labels, focus-visible, radio/checkbox semantics, no
   JS-only paths.
6. Consistent vocabulary: same action = same word; object naming by key or
   title, never row ids.

House patterns referenced below (all exist today, reuse — do not invent):

- **PRG success banner**: `<div class="alert alert-success" role="status"
  aria-live="polite">…</div>` rendered from a flash query key
  (src/web/subs.rs:63-84, src/web/keys_setup.rs:94-101, src/web/bookings.rs:87-99).
- **Mapped error keys → human copy** (never render the raw key):
  src/web/subs.rs:103-122, src/web/finance.rs:135-192.
- **Destructive confirm naming the object**:
  `onclick="return confirm('Revoke API key {name}?')"` — src/web/keys_setup.rs:29;
  money-grade: src/web/finance.rs:315-331.
- **One-time secret display** after create: src/web/keys_setup.rs:145-148.
- **focused_error_alert** (tabindex="-1" + data-focus-on-load):
  src/web/identity.rs:241-244.
- **Filter chips nav** with aria-current: src/web/subs.rs:184-218,
  src/web/bookings.rs:159-198.
- **Double-submit guard**: src/web/finance.rs:324, src/web/admin_console.rs:132.
- **Empty state** bold lead + help: src/web/content.rs:30, src/web/bookings.rs:230.
- **Type-DELETE confirm** for the strongest class: src/web/common_pages.rs:82.
- Button classes: `.btn` (primary fill, assets/app.css:388-405),
  `.btn-secondary` (:406), `.btn-danger` (:412), `.btn-ghost` (:414),
  `.btn-sm` (:416), `.input-num` (:353), `.inline-form` (:564).

**Flash-key vocabulary contract** (new keys must reuse these words):
`created | deleted | saved | revoked | extended | granted | deactivated |
activated | removed | approved | executed | error`. Banner copy pattern:
"{Object} {verb}." — e.g. "Plan deleted.", "Review approved."

---

## 1. Surfaces verified ALREADY GOOD — do not churn these

| Surface | Evidence |
|---|---|
| Finance approve/execute queues (the A12 reference) | Holds. Approve = `btn-secondary` + confirm naming amount/asset (finance.rs:315, 440); Execute = `btn-danger` + "real money" confirm (finance.rs:325, 450); double-submit guard (finance.rs:324, 449); honest banners incl. ambiguous-state and kick-warn (finance.rs:114-192). One Tier-3 nit only (§4). |
| Bookings confirm/decline/cancel | Holds. §3.5 matrix, decline/cancel `btn-danger` + confirm naming token + consequence (bookings.rs:393, 401); PRG banners with token + focus-on-load wrapper (bookings.rs:81-129); badge map (bookings.rs:16-23); truncation note (bookings.rs:350-357). |
| Subs revoke / extend / grant / expirecheck feedback | Holds. Rich honest banners incl. kick-failure warn and 0-case (subs.rs:50-128); revoke confirm names sub + consequence (subs.rs:276); expirecheck zero-case copy (subs.rs:54-56). |
| Subs filter/search/export toolbar | Good: chips with aria-current (subs.rs:191-213), search with hidden-status carry (subs.rs:315-332), pagination with Page x of y (subs.rs:334-380). |
| Keys issue/revoke | Good: one-time raw-key banner (keys_setup.rs:145-148), revoke confirm names key (keys_setup.rs:29), `?revoked=1` banner (keys_setup.rs:94-101), dead rows lose the button (keys_setup.rs:23-27). |
| Codes generate | Good: `Generated N code(s).` banner via `?id={n}` (codes.rs:117-121), defaults sane, clamp server-side (codes.rs:166-192). |
| AI provider/model save + test-send | Good: "Saved. AI settings apply on restart." — exactly the honest-scope bar (ai_actions.rs:151); server-side validation with rendered errors (ai_actions.rs:14-96, 114-116); test-send has noscript fallback + aria-live status (ai.rs:169-172, 184-201); write-only key with badge (ai.rs:63-98). Templates read-only with pointer (ai.rs:308-338) — deliberate, keep. |
| Monitor | Read-only, URL-state, typed errors, empty states (monitor.rs:24-315). No edit surface. Good. |
| Identity / wizard / auth (login, claim, MFA verify/enroll flows) | Good: focused_error_alert idiom (identity.rs:241-244), claim form re-render preserves token + focus (mfa_users.rs:324-350), one generic dead-link page (mfa_users.rs:305-316), login labels + autocomplete (auth_pages.rs:48-49). Two Tier-3 focus nits in §4. |
| Settings, bots, channels (prior waves) | Verified holding: settings banners + restart-required (settings.rs:88-118, 164), channels Edit-name inline w/ sr-only labels + Retarget/Delete confirms + row-added focus (channels_rows.rs:120, 66, 173), add-form disclosure (channels.rs:260). Skip. |
| Broadcast audience + confirm + send feedback | Good: live count note (admin_console.rs:95-105), "cannot be unsent" confirm + double-submit guard (admin_console.rs:106-109, 132), `?sent={n}` with 0-as-failure honesty (admin_console.rs:67-77). One Tier-1 flag on the segment form (E17). |
| Dashboard composer / post create | Good: channel select + note, honest typed `post_error` flash keys (dashboard.rs:29-31, 226-267). |
| Privacy delete | The strongest destructive pattern on the console (type-DELETE + confirm + `?deleted=1`): common_pages.rs:79-92, 153-199. Use as the model for any future hard-delete. |
| Posts page | Read-only list + pointer to composer (content.rs:119-129). No delete is honest (sent Telegram messages cannot be unsent) — keep as-is. |
| Customers / Carts rows | Read-only CRM and cart views; no edit affordances exist or are needed console-side (customers.rs:127-150, cart.rs:149-205). Cart add-by-id form is odd but cart is verify-only — noted §4, no item. |

---

## 2. Tier 1 — flow breaks (fix in one web pass)

Every item: evidence → heuristic broken → prescribed fix (markup/copy) →
size impact → at-risk tests. URL flash keys are chosen so existing pins stay
green; only where noted does a test need an additive case.

### E1. Plans: delete has no confirm, create is silent, errors render as raw keys
- Evidence: delete button one-click `btn-danger` with no confirm and no
  feedback — src/web/content.rs:37-45; `delete_plan_by_name` /
  `delete_plan_by_id` redirect plain (content.rs:450, 470). Success of create
  is silent (content.rs:406). The page banner prints the raw query value —
  `?error=invalid_duration` renders literally "invalid_duration"
  (content.rs:50-57, produced at :397, :400).
- Heuristics: 1 (destructive unconfirmed), 3 (no feedback, dishonest error
  copy), 6 (jargon on screen).
- Fix:
  a. Delete button gains `onclick="return confirm('Delete plan {name}? Buyers
     can no longer buy it.')"` (channels_rows.rs:66 idiom).
  b. Handlers redirect `?deleted=1` (name form keeps working; by-id form adds
     the same key); page renders `Plan deleted.` success banner via the same
     banner slot (extend `PlansQuery` with `deleted: Option<String>`).
  c. Create success redirects `?created=1` → `Plan created.` banner.
  d. Map the two error keys at render: `invalid_duration` → "Duration must be
     a positive number of days."; `missing_fields` → "Fill in name, price and
     asset." **Keep the URL keys** — tests/web_base_path.rs:405 pins
     `{BASE}/plans?error=invalid_duration`.
- Size: content.rs +14 (banner arm, confirm, two maps).
- Tests: web_base_path pin preserved. Additive: none required (render-only).

### E2. Products: "Deactivate" is a one-way dead end
- Evidence: row button Deactivate (content.rs:512) → `delete_product` sets
  active=0 (content.rs:632). `set_product_active` has **no `true` caller
  anywhere** (grep: src/api.rs:3605, src/handlers/commands.rs:1748,
  src/web/content.rs:632 — all false). An inactive product can never be sold
  again from any surface; badge says "inactive" forever (content.rs:509).
- Heuristics: 1 (weight: a reversible-sounding action is actually terminal),
  3 (silent mutation — plain redirect), 2 (dead end).
- Fix:
  a. Add `POST /product/activate` → `product_activate` handler: same scope
     rule as `delete_product` (content.rs:620-635), calls
     `set_product_active(seller, &name, true)`, redirects `?activated=1`.
     **New route justified**: it removes a dead end; reuses an existing Db
     method; no DB change.
  b. Row actions: inactive rows get `Activate` (`btn-secondary btn-sm`,
     aria-label `Activate {name}`); active rows keep Deactivate, now with
     `onclick="return confirm('Deactivate {name}? Buyers can no longer buy it
     in /shop.')"` and redirect `?deactivated=1` → banner `Product
     deactivated.` / `Product activated.`
- Size: content.rs +22 (handler + row branch + banner), mod.rs +1 route.
- Tests: additive — POST /product/activate flips the row and redirects with
  `activated=1` (pattern of src/web/tests/subs.rs).

### E3. Products: create failures are silent, DB error is a dead-end page
- Evidence: missing fields, bad kind, bad stock all plain-redirect
  (content.rs:573-578, 586-588) — the user's typed form is gone with no
  explanation; the DB-error path renders a bare error page with only a link
  (content.rs:599-609).
- Heuristics: 3 (honest feedback), 2 (inconsistent failure idioms).
- Fix: mirror Plans (E1d): add `error=missing_fields|bad_stock` keys to
  `ProductsQuery` with mapped copy ("Fill in name, price and asset." /
  "Stock must be a number, 0 or more."); replace the bare DB-error page with
  the products page + `focused_error_alert` carrying the DB reason
  (identity.rs:241-244 idiom, settings.rs:428 precedent). Success →
  `?created=1` → `Product created.`
- Size: content.rs +16, −4 (page replacement shrinks).
- Tests: none pinned; additive optional.

### E4. Codes: Reclaim destroys the old code and throws away the replacement
- Evidence: row form "Reclaim" `btn-secondary`, no confirm, carries a
  per-row "New length" number input (codes.rs:95-100). Handler discards the
  result: `let _ = state.db.reclaim_code(...)` (codes.rs:243) — and
  `reclaim_code` **revokes the old code and mints a new one**
  (src/db.rs:3181-3206), whose value is never shown. The operator invalidates
  a code they may have handed out and silently loses its replacement.
- Heuristics: 1 (destructive, unconfirmed, mis-weighted secondary), 3 (the
  outcome is invisible), 4 (per-row technical field nobody decides per row).
- Fix:
  a. Drop the inline length input (default 12 stays server-side).
  b. `btn-danger btn-sm` + `onclick="return confirm('Reclaim code {code}?
     The old code stops working and a new one is issued.')"` — destructive
     weight now matches the keys-revoke and promo-revoke class.
  c. Handler renders the keys_create one-time idiom in place (no PRG): banner
     `Code reclaimed. New code (shown once): <code class="copyable">{new}</code>`
     (keys_setup.rs:145-148 pattern; the new code comes from reclaim_code's
     `Option<String>`).
- Size: codes.rs +14, −2.
- Tests: additive — reclaim response body contains the new code and a
  "shown once" help line.

### E5. Codes: delete is silent; latent raw-error banner
- Evidence: delete has a good confirm naming the code (codes.rs:86) but
  redirects plain (codes.rs:212) — no banner. `codes_banner` renders
  `esc(e)` raw for `?error=` (codes.rs:122-127); no producer exists today,
  but the slot invites the Plans bug (E1).
- Heuristics: 3, 6.
- Fix: `codes_delete` redirects `?deleted=1` → `Code deleted.`; replace the
  raw-error arm with a mapped copy or delete the dead arm (prefer delete —
  no producer, YAGNI).
- Size: codes.rs +3, −6.
- Tests: none pinned.

### E6. Promos: create is fully silent, failures included
- Evidence: every validation failure (empty code, bad type, unparsable value)
  plain-redirects (promos.rs:127-129, 137-139, 140-143); success too
  (promos.rs:183). A typo'd value silently discards the whole form.
- Heuristics: 3 (worst-in-class: nothing is communicated).
- Fix: `PromosQuery { created, error }`; success → `?created=1` → `Promo code
  {code} created.` (code is the object key — include it); errors → mapped
  copy: "Enter a code.", "Choose percent or fixed.", "Value must be a number
  greater than 0."
- Size: promos.rs +16.
- Tests: none pinned.

### E7. Promos: revoke is confirmed but silent
- Evidence: confirm names the code (promos.rs:56) — then plain redirect
  (promos.rs:212).
- Heuristics: 3.
- Fix: redirect `?revoked=1` → `Promo code revoked.`
- Size: promos.rs +3.
- Tests: none pinned.

### E8. Orders: inline status select offers "refunded" beside the audited Finance flow
- Evidence: per-row select lists `refunded` (referrals.rs:110) while the same
  page's help copy says refunds go through Finance's create→approve→execute
  flow (referrals.rs:217), and bookings.rs:564-569 documents that the direct
  refund path was removed for exactly this reason. Inline "refunded" flips a
  label without the grant-revoke/kick or the audit trail — two paths, same
  word, different consequences.
- Heuristics: 1 (wrong action weight), 2 (divergent flows for one word), 6.
- Fix: remove `<option value="refunded">` from the inline select
  (referrals.rs:110); the row already links live orders to the pre-filled
  Finance refund form (referrals.rs:99-107). Keep `refunded` accepted by
  `orders_status` (REST/bot parity) — this is a UI vocabulary fix, not a
  behavior removal.
- Size: referrals.rs −1 line of markup.
- Tests: none pinned on the option list.

### E9. Reviews: approve gives no feedback
- Evidence: one-click Approve (referrals.rs:260) → `reviews_approve` → plain
  redirect (referrals.rs:313). The row vanishes from the pending list with
  zero acknowledgment — indistinguishable from a failed click.
- Heuristics: 3.
- Fix: `reviews_page` takes `Query(SubsQuery)` (the struct already carries
  `saved`, subs.rs:36); `reviews_approve` redirects `?saved=1`; page renders
  `Review approved.` (subs.rs:63-66 markup).
- Size: referrals.rs +6.
- Tests: none pinned.

### E10. Subs bulk-revoke is a destructive action reachable by GET
- Evidence: route registered `get(subs_bulkrevoke).post(subs_bulkrevoke)`
  (src/web/mod.rs:281-284); the handler kicks up to 1000 buyers via Telegram
  ban+unban then revokes (subs.rs:726-789). GET mutations are linkable /
  prefetchable — the rest of the console is POST-only for mutations (every
  other destructive action is a `method="post"` form).
- Heuristics: 1 (no confirm, no isolation), 5 (violates the no-JS/POST
  discipline the house keeps everywhere else).
- Fix: drop the GET arm — `.route("/subs/bulkrevoke", post(subs_bulkrevoke))`.
  No new route; no UI change (nothing renders a button for it today; when it
  gets one it must be `btn-danger` + confirm naming the status filter).
- Size: mod.rs −1/+1.
- Tests: additive — GET /subs/bulkrevoke now 404/405s (assert in
  src/web/tests/subs.rs style).

### E11. Web users: every mutation is silent and failures are swallowed
- Evidence: create `let _ = create_web_account(...)` (mfa_users.rs:730-734) —
  a duplicate username silently does nothing; password reset
  (mfa_users.rs:769-772), MFA reset (mfa_users.rs:609-612) and delete
  (mfa_users.rs:750-753) all plain-redirect. No flash keys exist on the page
  at all.
- Heuristics: 3 (admin surface with zero feedback — the least forgivable
  place), 2.
- Fix: `WebUsersQuery { created, error, deleted, password, mfa }`; create
  maps the Db error to "That username is taken." (`error=taken`), success
  `?created=1` → `Account {username} created.`; delete → `?deleted=1` →
  `Account deleted.`; password → `?password=1` → `Password reset.`; MFA
  reset → `?mfa=1` → `MFA disabled for {username}.` (username resolved from
  the row before the redirect, or pass the username as a non-sensitive flash
  param the way bookings carries `b`).
- Size: mfa_users.rs +28 (query struct, 4 banner arms, handler param picks).
- Tests: additive — duplicate create renders the taken-username banner.

### E12. Web users: "Reset MFA" is a primary-weight one-click security action
- Evidence: `btn btn-sm` (primary fill, app.css:388) with no confirm, next to
  the row's other actions (mfa_users.rs:643); handler disables TOTP
  immediately (mfa_users.rs:609-612). Disabling someone's second factor is
  the same class as keys revoke, not "extend".
- Heuristics: 1.
- Fix: `btn-danger btn-sm` (it strips protection; danger matches the
  outcome's risk, not database destructiveness — same reasoning as Execute
  being danger) + `onclick="return confirm('Reset MFA for {username}? They
  will sign in with password only until they enroll again.')"`; banner from
  E11.
- Size: mfa_users.rs +2.
- Tests: none pinned.

### E13. Web users: create asks for a raw seller id
- Evidence: `Seller id` free number input, placeholder "seller id (optional)"
  (mfa_users.rs:681-682). The owner cannot know seller ids; a typo silently
  mis-scopes the new account.
- Heuristics: 4 (decision field in a form that shouldn't need it now), 6
  (internal id as user input).
- Fix: replace the number input with a `<select id="wu-seller-id"
  name="seller_id">` whose first option is `value=""` → "No seller
  (platform-level)" and the rest are the seller ids with channels:
  `list_sellers()` + `seller_channel()` (the sellers_page idiom,
  admin_console.rs:406-422). Handler treats empty as None (already does,
  mfa_users.rs:726-729). Datalist fallback not needed — the set is small.
- Size: mfa_users.rs +10, −2.
- Tests: additive — create with empty seller_id still succeeds as None.

### E14. Admins: add/remove are silent
- Evidence: `admins_add`/`admins_remove` plain-redirect (admin_console.rs:370,
  389); no banners on the page. Remove's confirm names the **uid** not the
  username the row displays (admin_console.rs:307) — Tier-2 naming, fix in
  the same touch.
- Heuristics: 3, 6.
- Fix: `AdminsQuery { added, removed, error }` → `Admin added.` /
  `Admin removed.`; confirm copy becomes `Remove admin {username}? They lose
  console admin powers.` (username is in the row, admin_console.rs:312).
- Size: admin_console.rs +12.
- Tests: none pinned.

### E15. Sellers: "Upsert seller" jargon + two forms that both write the channel
- Evidence: panels "Upsert seller" (admin_console.rs:433-439) and "Set seller
  channel" (admin_console.rs:440-445) overlap — upsert already takes channel;
  two Saves write the same field with different copy. Handlers silent
  (admin_console.rs:485, 504). "Upsert" is developer vocabulary (heuristic 6,
  DESIGN.md "avoid jargon").
- Heuristics: 2 (ambiguous save — which form owns the channel?), 3, 6.
- Fix:
  a. Rename panels + buttons: "Add or update seller" → submit `Save`; "Set
     seller channel" → submit `Save` (same word, same verb class).
  b. Add one-line help under each: "Creates the seller if the id is new,
     otherwise updates name and channel." / "Updates only the channel."
  c. `SellersQuery { saved, error }` → `Seller saved.` / "Seller id must be a
     number." for the parse-fail silent drops (admin_console.rs:480, 501).
- Size: admin_console.rs +14.
- Tests: none pinned.

### E16. Crypto: "Show me" is a dead action (F2 from VISUAL_POLISH_SPEC §F — still open)
- Evidence: button posts to `/crypto/me` (admin_console.rs:225); the handler
  computes `msg` then discards it — `let _ = msg;` (admin_console.rs:278-284)
  — and redirects back with no flash. The owner clicks, nothing happens.
  VISUAL_POLISH_SPEC §F item 2 flagged this as out-of-scope there; it is in
  scope here.
- Heuristics: 3 (a mutation-shaped button with literally no feedback).
- Fix: `crypto_me` renders the AI-page pattern in place (no PRG, no
  free-text in URLs): success → `alert-success` "Crypto Pay reachable."
  + `Me: {esc(truncated json, 200 chars)}`; failure → `alert-error`
  "Crypto Pay check failed: {categorized}" (reuse the redact/categorize
  discipline from ai.rs:372-395 if the body is shown; prefer showing only
  ok/error-name). Re-render through `crypto_page` with a `banner` param —
  the keys_create (keys_setup.rs:149-155) direct-Html precedent.
- Size: admin_console.rs +18.
- Tests: additive — crypto_me with no token redirects (existing behavior)
  and page render contains the banner.

### E17. Broadcast: two `name="segment"` inputs in one form (F3 from VISUAL_POLISH_SPEC §F — still open)
- Evidence: the GET preview form renders both the preset select and the
  free-text input with the same name (admin_console.rs:128-129); both submit,
  and which one wins depends on form serialization order — a user who picks a
  preset and types custom text gets unpredictable audiences. Flagged in
  VISUAL_POLISH_SPEC §F item 3; verified still present.
- Heuristics: 2 (ambiguous form semantics), 5 (confusing without JS).
- Fix: one select whose last option is `value="custom"` labeled "Custom
  segment…" plus a `<details><summary>Custom segment</summary>` containing
  the text input named `segment` with `placeholder="product:1 / tag:vip /
  active:7 / minspend:10"`. The details is closed unless `?segment=` is
  non-preset. The POST form keeps its single hidden segment
  (admin_console.rs:133) — untouched. No JS needed; details is native.
- Size: admin_console.rs +8, −4.
- Tests: additive — GET with preset + custom renders only one visible
  `name="segment"` text input inside details.

### E18. Crypto: token save is silent
- Evidence: `crypto_setup` plain-redirect (admin_console.rs:257); the page's
  only signal is the badge flipping to "configured" (admin_console.rs:204-214)
  — re-render feedback, but weaker than every sibling surface and easy to
  miss next to the dead "Show me" (E16).
- Heuristics: 3.
- Fix: redirect `?saved=1` → `Token saved.` (blank-token submit is a no-op —
  keep honest by rendering nothing when nothing changed: only set the key
  when a non-empty token arrived, which the handler already does,
  admin_console.rs:254-256).
- Size: admin_console.rs +5.
- Tests: none pinned.

**Tier-1 count: 18.** Top 10 by severity if the pass must be cut:
E4 (lost code), E2 (dead-end products), E10 (GET destructive), E11 (silent
admin users), E8 (refunded divergence), E1 (plans delete/raw keys), E6
(silent promos), E16 (dead Show me), E17 (dual segment), E9 (silent reviews).

---

## 3. Tier 2 — consistency quick wins (land in the same pass where the file is already open)

### C1. Orders row verbs: "Set"/"Track" → "Save"; tracking input copy
- Evidence: referrals.rs:110-111. Every other inline edit says Save/Revoke/
  Delete; "Track" reads as "view tracking". Placeholder is lowercase jargon
  "tracking".
- Fix: both submit buttons → `Save`; placeholder → `Tracking number`
  (aria-labels already name the order — keep, they're fine). No handler
  change (`?saved=1` banner already exists, referrals.rs:209-213).
- Size: ±2. Tests: none pinned.

### C2. Referrals table: "Status" and "State" columns are the same fact twice (F5 — now ruled)
- Evidence: referrals.rs:35-46 print r.5 raw AND as a badge; headers at :53.
- Fix: drop the raw status `<td>`; keep the badge column, rename header
  "State" → "Status". colspan 7→6. The badge IS the dot-before-word fact
  (DESIGN.md §5).
- Size: ±3. Tests: none pinned.

### C3. Subs grant form: visible labels (aria-label-only inputs)
- Evidence: subs.rs:382-393 — three bare inputs with aria-labels; every other
  create form on the console uses `<label for>`.
- Fix: `<label for="g-user">Telegram user id</label>` etc. (three labels,
  same inputs). Panel keeps one primary `Grant` button — good today.
- Size: +3. Tests: none pinned.

### C4. Subs Extend weight drift
- Evidence: Extend button is default primary in every row (subs.rs:286)
  while the sibling finance Approve is `btn-secondary` (finance.rs:315).
- Fix: `btn btn-secondary btn-sm` (keep its confirm — it mutates access).
  Bookings Confirm keeps primary deliberately: it is the one primary action
  a held row offers (bookings.rs:391-397) — document, don't change.
- Size: ±1. Tests: none pinned.

### C5. Tickets: vocabulary, chips, banner
- Evidence: free-text status filter (customers.rs:231-238) vs the chips idiom
  everywhere else; "Set status" button (customers.rs:290); status change and
  reply re-render with no banner (customers.rs:361, 385).
- Fix: replace the filter input with the bookings chips nav (open/pending/
  resolved/closed/escalated + all, customers.rs:154-161 is the badge map);
  button → `Save`; `TicketsQuery` gains `saved` → `Ticket updated.` on both
  handlers' redirects.
- Size: +18, −4. Tests: none pinned.

### C6. Customers page: status filter as chips
- Evidence: free-text `Status` input (customers.rs:50) — the values come from
  a small known set (vip/regular/etc., customers.rs:134-137).
- Fix: chips nav for the known statuses + keep Tag/Search inputs (free-form
  by nature). Filter button ghost → drop (chips are links).
- Size: +10, −3. Tests: none pinned.

### C7. Products kind select: human labels
- Evidence: raw enum values as option text (content.rs:538).
- Fix: `<option value="digital_link">Link</option><option
  value="digital_file">File</option><option value="physical">Physical item
  (shipped)</option>` — values unchanged.
- Size: ±1. Tests: none pinned.

### C8. Promos "Days" and Type copy
- Evidence: unlabeled semantics `Days` value=30 (promos.rs:93), options
  percent/fixed raw (promos.rs:89).
- Fix: label → `Expires after (days)` + help "0 = never expires."; options →
  `percent (% off)` / `fixed (amount off)` — values unchanged.
- Size: +2. Tests: none pinned.

### C9. Web users delete confirm names the id, not the username
- Evidence: `confirm('Delete account 3?')` — uid interpolated
  (mfa_users.rs:642) while the username is right there in the row.
- Fix: `confirm('Delete account {username}? Their sessions end immediately.')`
  + aria-label `Delete account {username}`. (Land with E11/E12.)
- Size: ±1. Tests: none pinned.

### C10. Dashboard "Create plan" duplicates the Plans page form
- Evidence: identical wall-of-fields markup at dashboard.rs:88-96 and
  content.rs:60-66 — copy drift risk.
- Fix: apply E1's banner/confirm work to Plans only; for the dashboard panel
  either (a) mirror the exact markup whenever Plans form markup changes, or
  (b) preferred: shrink the dashboard panel to the plans list + a `Create on
  the Plans page` ghost link. (b) removes a duplicate Save from the dashboard
  — two primaries for one task (heuristic 1). the web layer may take (b) as the
  tie-breaker.
- Size: (a) 0, (b) −8/+2 in dashboard.rs. Tests: none pinned.

---

## 4. Tier 3 — niceties (only if the file is open anyway)

- N1. Keys table `aria-label="Table"` is generic — use "API keys"
  (keys_setup.rs:47).
- N2. Reviews empty state is a bare "No reviews." — use the bold+help idiom:
  `<p class="empty"><b>No reviews to moderate.</b> Buyer reviews appear here
  after delivery.</p>` + approve aria-label names product/buyer, not rid
  (referrals.rs:260, 271).
- N3. Finance payout/refund Amount inputs: `type="text"` without
  `inputmode="decimal"` (finance.rs:258, 280) — mobile keyboards.
- N4. MFA verify/enroll error alerts lack the focus-on-load treatment their
  sibling claim page has (mfa_users.rs:124-129, 213-218 vs mfa_users.rs:328).
- N5. Subs search input uses aria-label; monitor uses `sr-only` label
  (subs.rs:318 vs monitor.rs:90) — align on the sr-only label.
- N6. Cart "Add item" form requires a raw item id (cart.rs:63-65) — fine for
  a power-user utility, but a one-line help ("The id from /shop or the
  Products page") would stop the guesswork. Verify-only surface; lowest
  priority on this list.

---

## 5. Constraints & budgets

- **channels.rs (783 lines, near cap)**: untouched — no Tier-1 item lives
  there. bots files untouched. settings.rs untouched.
- **subs.rs (789)**: C3/C4 only; net +4. If the cap is enforced at 800, take
  C4 (±1) and defer C3 — Extend weight is the user-visible drift.
- **mfa_users.rs (773)**: E11-E13 + C9 land ~+40 → ~815. Accept the
  overshoot (Tier-1 admin feedback outranks the soft cap) or split the four
  banner arms into one `webusers_banner(q) -> String` helper to stay near
  ~805.
- **No DB changes**: everything reuses existing Db methods
  (`set_product_active` already supports reactivation; `reclaim_code` already
  returns the new code).
- **New routes: exactly one** — `POST /product/activate` (E2, justified:
  dead-end removal, existing Db method). One route *narrows*:
  `/subs/bulkrevoke` loses its GET arm (E10). No other route work.
- **esc()**: every new interpolated value (`{name}`, `{code}`, `{username}`,
  `{new}`) passes through `esc()`; flash keys are matched with `as_deref() ==
  Some("…")` like the house does — no free text rides URLs except the
  existing non-sensitive object keys (bookings `b` precedent,
  bookings.rs:57-59).
- **Additive tests only where behavior changes**: E2 (activate route), E4
  (reclaim banner), E10 (GET gone), E11 (taken-username banner), E17 (single
  segment input). Everything else is render-only.
- **At-risk pins (verified by grep — keep green)**:
  - tests/web_base_path.rs:405 pins `{BASE}/plans?error=invalid_duration` —
    E1 keeps the URL key, maps only the rendered copy.
  - src/web/tests/subs.rs:88, 170 pin `revoked=1` and
    `/subs?revoked=1&warn=kick` — no subs flash key changes.
  - src/web/smoke_tests.rs:130-142, 763 hit `/webusers`, `/promos` as 200s —
    page-only changes hold.
  - No test pins "Set", "Track", "Deactivate", "Upsert", "Show me", or any
    banner copy touched here (grepped tests/ + src/web/*tests*).

## 6. Suggested pass order for web (file-clustered, one pass)

1. content.rs (E1-E3, C7) — 635 → ~725.
2. admin_console.rs (E14-E18) — 505 → ~560.
3. mfa_users.rs (E11-E13, C9) — 773 → ~815 (or ~805 with the helper).
4. referrals.rs (E8, E9, C1, C2, N2) — 378 → ~420.
5. codes.rs + promos.rs (E4-E7, C8) — 245+213 → ~285+240.
6. customers.rs (C5, C6) — 386 → ~410.
7. mod.rs (E10 route arm, E2 route) — ±0.
8. subs.rs (C3, C4) — cap-aware, optional order.
9. dashboard.rs (C10b, optional) — 370 → ~364.
