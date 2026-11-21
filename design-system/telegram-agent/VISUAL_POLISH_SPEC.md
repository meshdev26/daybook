# VISUAL_POLISH_SPEC — console visual quality pass (wave 2)

**Discipline:** design · **Date:** 2026-10-31 · **Status:** ready for web
**Scope:** visual inconsistency, hierarchy, spacing, alignment, responsive
behavior, a11y floor. Flow/honesty/copy were fixed in wave 1
(`SETUP_UX_POLISH_SPEC.md`) and are NOT re-litigated here.

**Sources of truth (in order):** `DESIGN.md`, then `assets/app.css` (616 lines,
the real token system). `design-system/telegram-agent/MASTER.md` is
**non-authoritative and stale**: it specifies a slate/navy palette
(`#0F172A`, `#16A34A`, …), Fira Code/Fira Sans via a Google Fonts
`@import` (MASTER.md lines 40–52). The shipped console uses bundled Plex
Sans/Plex Mono with the ledger-green token set (app.css lines 16–101; DESIGN.md
§4). Web must treat MASTER.md and `design-system/telegram-agent/pages/*.md` as
historical reference only; see §D3.

**Evidence convention:** every claim cites `file:line` as of this writing.
All class names used below already exist in `assets/app.css` unless the item
explicitly writes new CSS.

---

## A. Global fixes (apply first — these propagate to every page)

### A1. Style `.sub` standalone — fixes the header of ~30 pages at once
**Wrong:** every page builder writes `<h1>Title <span class="sub">subtitle</span></h1>`
(dashboard.rs:72, subs.rs:470, bookings.rs:359, finance.rs:587, settings.rs:170,
channels.rs:175/227, codes.rs:60, promos.rs:99, content.rs:58/122/523(no sub),
referrals.rs:60/215/278, customers.rs:66/230/300, admin_console.rs:125/218/329/428,
keys_setup.rs:65, cart.rs:56/197, ai.rs:347, analytics.rs:210, identity.rs:380,
monitor.rs:249, mfa_users.rs:36/156/666, bookings.rs:359, settings.rs:399 …)
but `.sub` is styled **only** inside the never-used `.page-head`
(app.css:200–203; grep confirms `page-head` appears in no Rust file). So every
subtitle renders at h1 size (21px), weight 600, ink color — a run-on mega-title.

**Fix (app.css, add after the h1/h2/h3 block ~line 239):**
```css
/* Standalone subtitle: inside an h1 it de-emphasizes the run-on title;
 * bare (div.sub/h2.sub) it labels a fact line. h2.sub usages are being
 * removed per-page (B4/B5/B11) — this rule is the safety net. */
.sub {
  color: var(--ink-2);
  font-size: 13.5px;
  font-weight: 400;
  letter-spacing: 0;
}
```
Leave `.page-head` in place (harmless; a future header component may adopt it).

### A2. Shell: remove the duplicated page-title line
**Wrong:** `html_page` renders `<nav aria-label="Breadcrumb">{t}</nav>`
(html.rs:208) directly above the body, whose first element is the page's own
`<h1>` with the same word — every page shows its title twice (a plain 14.5px
line, then the 21px h1). The "breadcrumb" is flat (no links, no hierarchy).

**Fix (html.rs:208):** delete the line `<nav aria-label="Breadcrumb">{t}</nav>`
from the `html_page` template. The `<title>` element keeps the document name.
No test asserts this markup (grep of `src/web/tests/`, `*_tests.rs` — zero
matches for "Breadcrumb"). If the web layer prefers a zero-DOM-change variant, keep the
nav and add `nav[aria-label="Breadcrumb"] { font-size: 11px; font-weight: 500;
color: var(--ink-3); text-transform: uppercase; letter-spacing: 0.05em;
margin-bottom: 8px; }` — but removal is the prescribed fix; a caption that
duplicates the h1 adds nothing.

### A3. Shell: Settings appears twice in the rail; Log out dangles mid-list
**Wrong:** `rail_nav` appends `<a …>Settings</a>` and `<a …>Log out</a>` after
the nav groups (html.rs:478–479) while the `.foot` already carries a Settings
link (html.rs:205) — "Settings" renders twice per rail, and "Log out" sits
between the Manage group and the foot like a stray section item.

**Fix (html.rs):**
- rail_nav: delete lines 478–479 (both trailing links; return `out` after the
  Manage group).
- `.foot` template (html.rs:205) becomes:
```html
<div class="foot"><a href="{base}/settings">Settings</a> <a href="{base}/logout">Log out</a> <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()">Theme</button></div>
```
No test asserts "Log out" or the foot markup.

### A4. Shell: unlock banner + identity flash render full-bleed while content is capped
**Wrong:** `{unlock_banner}{identity_flash}` sit **outside** the `.measure`
wrapper (html.rs:207: `<main …>{unlock_banner}{identity_flash}<div class="measure">`).
On viewports wider than 1040px + padding, the standing identity-unlock banner
and the "Identity unlocked." flash stretch wider than every content panel below
them — the page reads as two different column widths.

**Fix (html.rs:207):** move both placeholders inside the measure div:
```html
<main class="main" id="main"><div class="measure">{unlock_banner}{identity_flash}
```
(closing `</div></main>` unchanged). The banner's own `.alert-form` layout
(app.css:607–608) is width-agnostic; nothing else moves.

### A5. Front door: the Theme button floats between the brand and the card
**Wrong:** `html_page_bare_panel` renders brand → `<button … theme-toggle>` →
panel (html.rs:243–244) inside a centered flex column, so a small ghost button
sits dead-center above every login/claim/setup/wizard card.

**Fix (app.css, add to the front-door block ~line 221):**
```css
.door { position: relative; }
.door .theme-toggle { position: absolute; top: 16px; right: 16px; }
```
No HTML change; the button stays keyboard-reachable (it is after the skip link
in DOM order).

### A6. `<details>` blocks have zero margins and half-nest their panel
**Wrong:** `details { … }` (app.css:498) sets no margin, so the four stacked
settings sections (settings.rs:170–176: connection, password, MFA, management
`<details open>` siblings) touch border-to-border — one merged blob. Inside
them, `details > article.panel` keeps `margin-bottom:16px` (app.css:224–230),
producing a stray 16px recessed strip below each white card and a 10px-radius
card butted against a 7px-radius track.

**Fix (app.css, add to the details block ~line 510):**
```css
details { margin: 0 0 12px; }
li details { margin-bottom: 0; }            /* AI recent-errors rows keep tight */
/* Settings accordion body: the white card attaches flat to its summary. */
details > article.panel {
  margin: 0;
  border: 0;
  border-top: 1px solid var(--line);
  border-radius: 0;
}
```

### A7. New utility `.grid-stats` for count-card rows
**Wrong:** bookings renders 4 count cards in `.grid-2` (bookings.rs:145–150),
analytics 6 (analytics.rs:212–219), subs 5 (subs.rs:167–173) — two cards per
row with the odd card orphaned, and five/six stacked full-width rows on phones.

**Fix (app.css, add near `.grid-2` ~line 513):**
```css
/* Metric-card rows: auto-fit keeps cards equal-width at any count. */
.grid-stats {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 16px;
  align-items: stretch;
  margin-bottom: 16px;
}
.grid-stats .panel { margin-bottom: 0; }
```
Per-page consumption in B3/B4/B11.

### A8. Kill the three inline `style=` attributes with one utility
**Wrong:** `style="width:4.5em"` on the subs Extend input (subs.rs:280),
`style="width:4em"` on the codes Reclaim input (codes.rs:95),
`style="margin-top:1rem"` on the settings Restart panel (settings.rs:166).
Inline styles bypass the token system and drift (4 vs 4.5em).

**Fix (app.css, add to the forms block ~line 322):**
```css
/* Compact numeric entry inside table rows and inline forms. */
.input-num { width: 4.5em; min-width: 4.5em; margin-top: 0; }
```
Then: subs.rs:280 `style="width:4.5em"` → `class="input-num"`;
codes.rs:95 `style="width:4em"` → `class="input-num"`;
settings.rs:166 delete `style="margin-top:1rem"` (A6's details margin +
`.panel` margin-bottom give the spacing back). Note `.inline-form input` sets
`margin: 5px 0 2px` via the global input rule; `margin-top: 0` here keeps the
chip row vertically centered — if web finds baseline drift, drop that one
declaration.

### A9. Guard `a.panel` against the global link underline
**Wrong:** `.panel` is used as an `<a>` on Settings → Management quick cards
(settings.rs:355–359). Global `a:hover { text-decoration: underline }`
(app.css:115) underlines the whole card's text on hover.

**Fix (app.css, add to the panels block ~line 233):**
```css
a.panel { color: inherit; text-decoration: none; }
a.panel:hover { text-decoration: none; }
a.panel:hover h3 { color: var(--brand); }
```

### A10. Mobile rail: the bot-scope control is a tall column in a horizontal bar
**Wrong:** ≤900px the rail becomes a horizontal bar (app.css:580–597) but
`.rail .bot-scope` stays `flex-direction: column` (app.css:170–177) — on a
phone the Bot selector + Switch button stack as a tall narrow block in the top
bar, pushing the nav down. Same for the pinned identity line
(`bot-scope-pinned`, identity.rs:129) which inherits the column layout.

**Fix (app.css, add inside the existing `@media (max-width: 900px)` block
~line 597):**
```css
  .rail .bot-scope {
    flex-direction: row; align-items: center;
    flex: 1 1 100%; gap: 8px;
    padding: 4px 0 8px;
  }
  .rail .bot-scope select { flex: 1 1 auto; width: auto; min-width: 8rem; }
  .rail .bot-scope .btn { align-self: center; }
```

### A11. `.kv` label column is 160px fixed — cramped on phones
**Wrong:** `.kv { grid-template-columns: 160px 1fr }` (app.css:538) leaves
~130px for values on a 320px viewport (Identity status panel, identity.rs:421).

**Fix (app.css, add to the responsive block):**
```css
  @media (max-width: 900px) { .kv { grid-template-columns: 1fr; gap: 2px 0; } .kv dt { margin-top: 8px; } }
```

### A12. Row-action button variant rule (calm the sea of green)
**Wrong:** neutral row mutations render as **primary brand-green** buttons:
orders "Set"/"Track" (referrals.rs:110–111), codes "Reclaim" (codes.rs:95),
reviews "Approve" (referrals.rs:260), plans "Delete" is fine (danger). The
Finance queues already model the correct idiom: Approve = `btn-secondary`,
Execute = `btn-danger` (finance.rs:315, 325, 440, 450). Every page carrying a
dozen green chips breaks the "no hover-lift theatre / calm" ledger contract
(DESIGN.md §3) and flattens the page's real primary action.

**Rule for web (instances listed in B):** in tables/lists, neutral row actions
use `btn btn-secondary btn-sm`; destructive uses `btn btn-danger btn-sm`; the
**page's** primary CTA (the one create/save button) stays plain `btn`.
Confirm/primary-pair rows that are THE action of the row (bookings Confirm,
finance Execute) may keep their current emphasis.

---

## B. Per-page punch list (ordered by operator traffic)

Canonical page order (enforced where a page deviates):
`h1 → banners → primary form/CTA → filter toolbar → table → pagination → footnotes`.
Empty states: `<b>No X yet.</b>` + one "how this fills" line (the subs/bookings
idiom, subs.rs:242, bookings.rs:230).

### B1. Dashboard — `src/web/dashboard.rs`
1. **Recent-subscriptions rows are unstyled text blobs** (dashboard.rs:163:
   `<li>{id} user {user} {channel} expires {date}</li>`), while the sibling
   Plans list on the same page is structured (dashboard.rs:146: bold name +
   mono price + help). Fix the row shape to:
   `<li><span class="mono">#{}</span> user <span class="mono">{}</span> <span class="mono">{}</span> <span class="help">expires {}</span></li>`
   (data unchanged; `esc()` as today).
2. **"Default channel" is a whole panel for one mono line** (dashboard.rs:78).
   Convert to the stat component so it matches bookings' counts:
   `<article class="panel"><div class="stat"><span class="stat-label">Default channel</span><span class="stat-value">{ch}</span></div></article>`
   (`.stat` is the DESIGN.md §5 metric component; 24px Plex Mono value).
3. **Stray closing `</label>`** on the Create-plan channel input
   (dashboard.rs:89 `…placeholder="@channel (optional)"></label>`) — invalid
   HTML after a closed label. Delete the trailing `</label>`. Same artifact in
   channels.rs:178 and content.rs:65 (see B7/B8).
4. **Onboarding checklist + banners read as one gray stack**: the checklist
   `li`s (dashboard.rs:346–348) are plain; give each `<li>` the existing
   `help`-free shape and keep as is except: wrap the three banner slots
   (welcome/post_error/connect/restart) so a banner-free dashboard doesn't
   leave a stray gap — they already render "" when absent; no change needed
   beyond A1/A4. (Low risk; included for completeness.)

### B2. Orders — `src/web/referrals.rs`
1. **Row-action buttons are primary green** (referrals.rs:110–111 "Set",
   "Track"): apply A12 → `class="btn btn-secondary btn-sm"`.
2. Reviews table: **anonymous Actions column** — the last `<th>` is empty
   (referrals.rs:274). Replace with
   `<th scope="col"><span class="sr-only">Actions</span></th>`, and give the
   Approve button `class="btn btn-secondary btn-sm"` plus
   `aria-label="Approve review {rid}"` (A12; screen-reader context — the same
   idiom channels.rs:305 already uses).
3. Referrals page: **"Bonus balance" one-liner panel** (referrals.rs:61) —
   convert to the B1.2 stat shape (`stat-label` "Bonus balance" /
   `stat-value` amount).
4. Referrals table "Status" and "State" are two columns (referrals.rs:53) —
   flag only (data question, not visual).

### B3. Subscriptions — `src/web/subs.rs`
1. **Count cards invert the hierarchy**: `div.sub` label + `div.mono` value
   (subs.rs:167–172) — with A1 applied the label is 13.5px but the **value
   stays 13px mono**: the number is smaller than its label. Rewrite
   `subs_count_cards` to the bookings shape (bookings.rs:146–149) inside the
   new container:
   ```html
   <section aria-labelledby="subs-counts-heading">
   <h2 id="subs-counts-heading">Status counts</h2>
   <div class="grid-stats">
   <div class="panel"><div class="stat"><span class="stat-label">Active</span><span class="stat-value">{active}</span></div></div>
   … Expired / Revoked / Refunded / Total …
   </div></section>
   ```
   (drop `class="sub"` from the h2 — see A1 note).
2. **Body order buries the table**: currently h1 → cards → filter → search →
   **pagination → grant form → table** (subs.rs:469–477) — "Page 1 of 3"
   appears above the table it paginates and the grant form splits filter from
   data. Reorder `subs_body` to: h1 → cards → toolbar (`<div
   class="inline-form">{filter_nav}{expire_check}</div>` + `{search_form}`)
   → grant form → table → **pagination** → footer. No handler changes.
3. **Pagination is a bare text node between ghost buttons** (subs.rs:354
   `" Page {} of {} "`): wrap it:
   `<nav class="inline-form" aria-label="Subscriptions pagination">{prev}<span class="help">Page {page} of {total_pages}</span>{next}</nav>`
4. Extend input: apply A8 (`class="input-num"`).
5. Filter chips: already the correct btn-secondary/btn-ghost + aria-current
   idiom — no change (reference pattern for other pages).

### B4. Bookings — `src/web/bookings.rs` (mostly correct — the reference page)
1. Counts: switch the wrapper `grid-2` → `grid-stats` (bookings.rs:145) and
   drop `class="sub"` from the h2 (A1 note); the `.stat` markup is already
   right.
2. Banner focus wrapper (bookings.rs:125 `<div tabindex="-1" autofocus>`) —
   valid but invisible; give it `role="status"` so the flash is announced:
   `<div tabindex="-1" autofocus role="status">`. (Copy untouched.)

### B5. Finance — `src/web/finance.rs`
1. Section headings use the `.sub` hack: `<h2 … class="sub">Payouts</h2>` /
   `Refunds` (finance.rs:594, 597). Remove `class="sub"` — plain h2 (16px/600)
   is the DESIGN.md section scale; with A1 they'd otherwise shrink to captions.
2. Tables/rows already follow the button idiom (A12 reference). No other
   changes.

### B6. Settings — `src/web/settings.rs`
1. Restart panel inline style → A8 (delete `style="margin-top:1rem"`,
   settings.rs:166).
2. The four `<details open>` sections get A6 spacing/flat-card treatment —
   no HTML change required.
3. MFA panel "Status: {badge}" / "Unused recovery codes: {n}" (settings.rs:316–317)
   read as loose sentences while the rest of the console uses the kv pattern:
   convert to `<dl class="kv"><dt>Status</dt><dd>{badge}</dd><dt>Unused recovery codes</dt><dd class="mono">{n}</dd></dl>`
   (matches identity.rs:421–425).

### B7. Channels — `src/web/channels.rs`
1. Stray `</label>` in the add-channel help (channels.rs:178) — delete the
   trailing `</label>`.
2. Seller view row (channels.rs:162) lacks the `.row-actions` wrapper the
   global view uses (channels.rs:347): wrap the delete form in
   `<div class="row-actions">…</div>` so both views align actions right the
   same way.

### B8. Plans / Products / Posts — `src/web/content.rs`
1. Plans page order deviates from canon (list panel then create form,
   content.rs:57–66) while codes/promos/finance put the form first. Swap:
   `{banner}<h1>…</h1><article class="panel"><h3>Create plan</h3>…</article><article class="panel"><ul>{list_items}</ul></article>`.
2. Products header has no sub: `<h1>Products</h1>` (content.rs:523) — every
   sibling page carries one. Add `<span class="sub">digital delivery</span>`
   (copy: visual-parity fix; not test-asserted).
3. Products create form uses `article.card` — the only `.card` in the codebase
   (content.rs:525); use `article.panel` (rendered identically today, app.css:231,
   but the drift invites divergence).
4. Products/Reviews anonymous `<th></th>` (content.rs:518) →
   `<th scope="col"><span class="sr-only">Actions</span></th>`; add
   `aria-label="Deactivate {name}"` to the Deactivate button (content.rs:504)
   and `aria-label="Delete plan {name}"` to the plans Delete button
   (content.rs:37) — both currently anonymous among siblings.
5. Bare empty states get the standard shape: products (content.rs:487) →
   `<p class="empty"><b>No products yet.</b> Create your first one below — buyers see it in /shop.</p>`;
   posts (content.rs:90) already has guidance — keep.
6. Posts page sub "recent posts" is a tautology (`Posts — recent posts`,
   content.rs:122): change to `<span class="sub">recent sends</span>`
   (copy edit required by inconsistency, not flow).

### B9. Codes / Promos / Referrals program — codes.rs, promos.rs, referrals.rs
1. Codes "Reclaim" → `btn btn-secondary btn-sm` (A12; codes.rs:95) + A8
   `.input-num`.
2. Codes Delete button gets `aria-label="Delete code {code}"` (codes.rs:86 —
   confirm() names it, the button doesn't).
3. Promos Revoke button gets `aria-label="Revoke promo code {code}"`
   (promos.rs:56).
4. Empty states to the standard shape: codes (codes.rs:74) →
   `<b>No codes yet.</b> Generate a batch above — hand them out with /redeem.`;
   promos (promos.rs:35) → `<b>No promo codes yet.</b> Create one above — buyers apply it in /buy.`;
   referrals (referrals.rs:30) → `<b>No referrals yet.</b> Buyers join with your referral code — rewards appear here.`;
   customers (customers.rs:129) → `<b>No customers yet.</b> They appear after a buyer's first purchase.`;
   tickets (customers.rs:197) → `<b>No tickets.</b> Buyer conversations opened with /support appear here.`.
   (None of these strings are test-asserted.)

### B10. Cart / Carts — `src/web/cart.rs`
1. "Clear cart" renders even when the cart is empty (cart.rs:64–66) — a
   destructive button pointing at nothing. Render it only when the table has
   rows (move it inside the non-empty branch of `table`).
2. Cart empty state (cart.rs:48) → `<p class="empty"><b>Your cart is empty.</b> Add an item above or browse /shop in the bot.</p>`;
   carts empty (cart.rs:189) → `<p class="empty"><b>No active carts.</b> Buyer carts appear here while they shop.</p>`.
3. Carts table renders buyer/seller ids as plain `td` (cart.rs:184) — ids are
   mono everywhere else (subs.rs:261): `class="mono"` on the two id cells.

### B11. Analytics — `src/web/analytics.rs`
1. **The period filter renders above the h1** (analytics.rs:206–210: form,
   then `<h1>Analytics…`). Move the form below the h1 — canon order.
2. **Metric cards use `div.sub` + `div.mono`** (analytics.rs:213–218) — same
   inverted hierarchy as B3.1. Rewrite the Overview section to the `.stat`
   shape in a `.grid-stats` (six cards, labels: Revenue total / Invoice counts
   / AOV / Customer count / Repeat buyers / Abandoned cart rate).
3. `<h2 … class="sub">Overview</h2>` / `Top products` (analytics.rs:211, 220):
   drop `class="sub"`.
4. **Chart.js renders in a default blue on a ledger-green console, and the
   script comes from a CDN** (analytics.rs:224
   `<script src="https://cdn.jsdelivr.net/npm/chart.js">`) — DESIGN.md/app.css
   promise "served from the binary — no CDN" (app.css:9). Two-part fix:
   a. (in scope) color the chart from tokens — replace the chart init
      (analytics.rs:226–237) with:
      ```js
      var cs = getComputedStyle(document.documentElement);
      var ink2 = cs.getPropertyValue('--ink-2').trim();
      var line = cs.getPropertyValue('--line').trim();
      var brand = cs.getPropertyValue('--brand').trim();
      new Chart(ctx, { type: 'bar',
        data: { labels: {labels}, datasets: [{ label: 'Sales', data: {values},
                 backgroundColor: brand, borderRadius: 3 }] },
        options: { plugins: { legend: { labels: { color: ink2 } } },
          scales: { x: { ticks: { color: ink2 }, grid: { color: line } },
                    y: { ticks: { color: ink2, precision: 0 }, grid: { color: line } } } } });
      ```
      and give the canvas `role="img" aria-label="Bar chart of sales by product; the table above lists the same data"`.
   b. (policy, see §F) whether the jsdelivr CDN may stay at all.

### B12. AI — `src/web/ai.rs`
1. **Panel-in-panel**: the Usage & cost section is a `.panel` wrapping a
   `grid-2` of `.panel` metric cards plus two more panels (ai.rs:291–301) —
   double borders and double padding. Demote the outer section to a bare
   `<section aria-labelledby="ai-usage-heading">` with an `<h2>Usage &amp; cost</h2>`
   (id stays), keeping the inner panels; the Today/7-day cards then convert to
   the `.stat` shape (B3.1 idiom) inside `.grid-stats`.
2. Templates panel is a `section.panel` (ai.rs:331) — fine; keep.

### B13. Identity / Monitor — identity.rs, monitor.rs
Already the most polished surfaces (kv panel, badges, honest empties). One item:
the rail identity line and unlock banner are covered by A4/A10; no page change.
Monitor's h2-free structure and `.timeline` are per IDENTITY_MONITOR_UX_SPEC —
leave alone.

### B14. Door pages: login / claim / setup / wizard / MFA / invite link
1. **Wizard step counters drift**: steps say "Step 1 of 5" … "Step 4 of 5"
   (wizard.rs:58/82/108/126), then "Step 5 of 6" (wizard.rs:143) and "Final
   step" (wizard.rs:161) — the math never reconciles. Prescribe: keep "of 5"
   on steps 1–4 (test asserts "Step 1 of 2" only on the Bots wizard, safe);
   change wizard.rs:143 `Step 5 of 6` → `Optional — only if your account has two-factor`;
   keep "Final step".
2. Login throttle dead link: `<a href="#">Back to sign in</a>`
   (auth_pages.rs:152) — points nowhere. Fix the href to the login URL
   (`state.url("/login")`; the route exists, no behavior change) and give it
   `class="btn btn-secondary"` to match the door's affordances.
3. "Restarting" interstitial hard-codes `href="/"` (settings.rs:401) — breaks
   under `WEB_BASE_PATH`. Change to `state.url("/dashboard")` with
   `class="btn"` (one-token fix; flagged in §F because it touches an href).
4. Invite-link one-time secret renders on the bare page background
   (channel_ops.rs:221–222): wrap the body's link block in
   `<article class="panel">…</article>` so `code.copyable` sits on a card like
   every other one-time secret (keys_setup.rs:146 idiom).

### B15. Web users / Admins / Sellers — mfa_users.rs, admin_console.rs
1. **Status words render raw where the console uses dot-before-word badges**:
   webusers MFA column "Enabled"/"Disabled" (mfa_users.rs:632) →
   `<span class="badge badge-success">enabled</span>` / `badge-warning` (the
   settings MFA badge idiom, settings.rs:309–311); tickets Status cell
   (customers.rs:203) → map open→`badge`, pending→`badge-warning`,
   resolved→`badge-success`, closed→`badge`, escalated→`badge-danger`;
   customers Status (customers.rs:135/142) → same vocabulary (vip/regular →
   default badge). Badge text stays the raw status word — color never alone
   (DESIGN.md §5).
2. **Sellers page ships duplicate element ids**: two forms both contain
   `id="user_id"`, `id="username"`, `id="channel"` (admin_console.rs:432–433 vs
   438–439) — every label after the first form binds to the wrong input
   (WCAG 1.3.1/4.1.1). Namespace them (`upsert-user-id`, `upsert-username`,
   `upsert-channel`, `setchan-user-id`, `setchan-channel`) and update the
   `for` attributes.
3. Privacy page has the same duplicate-id bug for both forms
   (`id="seller_id"`, `id="user_id"` in common_pages.rs:73–74 and 80–81) —
   namespace (`export-seller-id`, `export-user-id`, `delete-seller-id`,
   `delete-user-id`).

### B16. Bots — `src/web/bots_admin.rs` (FLAG-ONLY: file is over its size budget)
Report only; no edits unless a one-line class swap:
- F-1. `bot_card_html` renders Disable/Delete as two stacked separate
  `<form class="inline-form">` rows (bots_admin.rs:302–307) while every list
  page wraps its actions in `.row-actions` (channels.rs:347). One-line fix if
  allowed: wrap both forms in one `<div class="row-actions">`.
- F-2. The add-wizard token input is a bare `type="password"` without the
  `.secret-field` reveal toggle (bots_admin.rs:681) — the only secret field in
  the console without one (every other uses SECRET_TOGGLE, mod.rs:446).
- F-3. File size: 1128 lines; any further cleanup belongs to a budget pass.

### B17. Common pages — common_pages.rs
1. 404/forbidden h1s render fine; keep.
2. Privacy duplicate ids → B15.3.

---

## C. Accessibility floor (WCAG 2.1 AA)

### C1. Measured contrast — token pairs actually used (computed, both themes)
Light (on `--surface` unless noted): ink 16.67:1 · ink-2 6.21:1 · ink-3 5.57:1
· brand 6.40:1 · warn 5.91:1 · danger 6.47:1 · brand-ink/brand (primary btn)
5.90:1 · danger-ink/danger 6.47:1 · ink-2 on surface-2 5.98:1 · ink-2 on
brand-tint 5.34:1 · ink on warn-tint 14.35:1 · ink on danger-tint 14.10:1 ·
badge-success brand/brand-tint 5.51:1 · badge-warning warn/warn-tint 5.09:1 ·
badge-danger danger/danger-tint 5.47:1.
Dark: ink 14.44:1 · ink-2 7.61:1 · ink-3 5.63:1 · brand 7.42:1 · warn 7.94:1 ·
danger 5.85:1 · brand-ink/brand 8.03:1 · danger-ink/danger 6.34:1 · ink-2 on
tints 6.57–6.83:1.
**Every text pair passes 4.5:1 in both themes; every control/icon pair passes
3:1 (`--line-strong` 3.10 light / 3.67 dark; secret-toggle icon 5.57/5.63).**
The app.css header's claimed ratios are conservative — no color changes are
required. Note for the record: `--line-strong` on `--bg` paper is 2.86:1
(light) — below the 3:1 non-text floor; no current component draws a control
border on paper (inputs always sit on surfaces), so this is a *rule*, not a
defect: **never render inputs/buttons directly on `--bg`** (see §D2).

### C2. Contrast-independent failures found (all have fixes above)
1. Duplicate input ids break label association — Sellers page (B15.2),
   Privacy page (B15.3). WCAG 1.3.1, 4.1.1.
2. Unlabeled control: ticket status `<select name="status">` has no label and
   no aria-label (customers.rs:269–275). Fix: `aria-label="Set status for ticket {id}"`.
3. Anonymous row buttons (screen readers read "Delete, Delete, Delete"):
   plans (content.rs:37), products Deactivate (content.rs:504), codes Delete
   (codes.rs:86), promos Revoke (promos.rs:56), reviews Approve (referrals.rs:260).
   Fix: aria-label idiom already used by channels/codes-extend/subs — B8.4/B9.
4. Anonymous table action columns (`<th></th>`): products (content.rs:518),
   reviews (referrals.rs:274). Fix: sr-only "Actions" (B2.2/B8.4).
5. Generic region name on every table: `role="region" aria-label="Table"`
   (19 call sites, e.g. subs.rs:304, bookings.rs:246, finance.rs:397). Fix
   when touching each page: set the region's aria-label to the table's name
   ("Subscriptions", "Orders", …). Mechanical, low priority.
6. Heading order: Analytics renders a form before its h1 (B11.1) — the only
   page where the first heading is not an h1. Fixed by reordering.
7. Chart canvas has no accessible alternative of its own — acceptable because
   the "Top products" table carries the same data; add `role="img"` +
   aria-label (B11.4a).
8. `autofocus` on a plain div (bookings banner bookings.rs:125, MFA claim
   alert mfa_users.rs:328) works (both carry tabindex="-1") but is
   non-conforming HTML on a non-interactive element; the pattern is already
   the documented spec idiom (APP_JS focuses `.unlock-banner` /
   `[data-focus-on-load]` instead, html.rs:34–37). Prescribe: on the next
   touch, drop `autofocus` and add `data-focus-on-load` (same behavior, one
   attribute swap). Flag-level; do not batch-edit now.

### C3. Verified-OK (no action)
- Focus visibility: global `:focus-visible` 2px brand outline (app.css:116);
  inputs swap to brand border+ring (app.css:315–319); `.table-wrap:focus`
  outline (app.css:426); nav-group-toggle has its own rule (app.css:154).
- Keyboard: skip link on every shell page (html.rs:199, app.css:562–569);
  filter chips are real links; collapsible groups are buttons with
  aria-expanded/aria-controls and `inert` on collapsed panels (html.rs:62–73).
- Touch targets: 44px floors for rail links, btn-sm, secret-toggle at ≤900px
  (app.css:578, 351).
- Reduced motion: both nav animation and a global kill switch (app.css:166–168,
  598–601).
- Language/landmarks: `lang="en"`, single `<main id="main">`, rail `<nav>`.
- Dark mode: `data-theme` boot script before paint (html.rs:196) — no flash.

---

## D. Token contract — design SECOND OPINION REQUIRED

**D1. Verdict: this wave requires NO token additions or changes.** All new CSS
in §A is structural (layout/spacing) and reuses existing variables. Nothing in
§B introduces a color. Do not let web "improve" the palette while implementing.

**D2. Latent risk to put on the record (no code change):** `--line-strong` on
`--bg` is 2.86:1 in light mode — under the 3:1 non-text floor of WCAG 1.4.11.
Today every control sits on `--surface`/`--surface-2` (3.10:1 — passes). Rule
for future pages: any input/button drawn directly on the paper background
would fail; if such a page ever ships, the fix is a `--line-strong` darkening
pass at that time (token change, design again).

**D3. Documentation debt (owner decision, no code):** `MASTER.md` (and
`design-system/telegram-agent/pages/`) documents a different design system —
slate/navy palette, Fira Code/Fira Sans via Google Fonts CDN — directly
contradicting `DESIGN.md` §4 and `app.css` (bundled Plex, no CDN; MASTER.md
lines 40–52). It is marked non-authoritative at the top, but it is the first
hit for "design system". Recommend archiving it under `docs/reviews/` or
prepending a deprecation banner naming `DESIGN.md` + `app.css` as the only
sources of truth.

---

## E. Constraints for web

1. **No behavior changes**: no route, handler-logic, SQL, or flow edits.
   The only non-CSS/non-class edits this spec authorizes are the shell
   restructures in A2/A3/A4 (markup deletion/moves), the two href corrections
   in B14.2/B14.3 (dead link `#` and base-path-less `/` — same routes that
   already exist), and the attribute adds in B4.2/C2.8. Everything else is
   class/structure/ordering.
2. **Reuse before new**: every class name in this spec already exists except
   `.grid-stats`, `.input-num`, and the `a.panel` / `details` rules — which are
   written out in full in §A. Nothing else may be invented.
3. **Function bodies ≤100 lines**: `subs_body`, `subs_count_cards`,
   `analytics_page`, `ai_usage_dashboard` are touched — keep helpers extracted
   the way bookings.rs does (counts/filter/table split).
4. **`bots_admin.rs` is over budget**: B16 items are report-only.
5. **Copy edits** appear only where visual inconsistency demands (B8.2,
   B8.6, B9.4, B14.1) and none touch the asserted strings below.
6. **Quality gates** (AGENT.md §2) must pass: `cargo test`,
   `cargo clippy -- -D warnings`, `cargo fmt --check`.

### Test-affecting strings (from `src/web/tests/`, `*_tests.rs`) — do not alter
Copy: "multi-bot management" · "Bot settings saved — they apply on restart." ·
"Global channels" · "bot-managed" · "Step 1 of 2 — prove the token works" ·
"Step 2 of 2" · "Optional — add later in Settings" · "The bot is not connected
yet." · "No API key configured." · "restricted to the console owner" ·
"This page won't come back." · "Two steps. This page is step 1 — step 2 is
restarting the app." · "will go to <strong>1</strong>" · "Finish setup later
in Settings → Bot connection?" · "disabled — it stops responding after the
next restart." · "deleted from the console." · "That bot no longer exists —
nothing was changed." · "Password changed. Other sessions were signed out." ·
"Saved. The bot connects on restart." · "Saved. AI settings apply on restart." ·
"Choose a dashboard password" · "Passwords do not match" · "Invalid credentials" ·
"Claim the console" · "Claiming requires the one-time code" · "Paste the bot
token from @BotFather" · "Owner ID must be the number" · "Subscriptions".
Markup/class tokens: `alert-error` · `data-focus-on-load` ·
`bot-scope-pinned` · `bot-scope-select` · `<select name=` · `" selected>"` ·
`(default)` option · `<details open><summary>Optional` ·
`autocomplete=` · `owner_user_id` · `token_env = "`.
All prescribed changes avoid every string above (verified by grep on
2026-10-31); re-run `cargo test` after implementing anyway.

---

## F. Flags for the owner (reported, NOT in web's scope)

1. **Chart.js CDN** (analytics.rs:224) violates the "served from the binary —
   no CDN" contract (app.css:9) and adds a third-party script to an
   authenticated operator console (supply-chain + offline-desktop risk).
   Options: vendor the file into `assets/` like the fonts, render the bars as
   inline SVG (zero JS), or drop the chart (the table stays). Needs a ruling;
   B11.4a fixes the colors either way.
2. **Crypto "Show me" button has no visible effect** (admin_console.rs:225 →
   crypto_me at :261–285 computes the result into `msg` and discards it, then
   redirects back with no flash). Visual honesty problem; fixing it means a
   flash banner = handler change (out of this wave).
3. **Broadcast form has two `name="segment"` fields** (select + free text,
   admin_console.rs:128–129) — both submit and the outcome depends on
   serialization order. Flow bug from wave 1 territory; reported here because
   it also renders as two competing inputs in one row.
4. **Empty-cart "Clear cart"** (B10.1) — the render condition is a one-line
   handler-side `if`; kept in B as a structure move, but if the web layer prefers, treat
   as a flag.
5. **Referrals table has both "Status" and "State" columns** (referrals.rs:53)
   — likely a data-model leftover; needs a product decision.
6. **`nav-group-label`, `unlock-banner`, `theme-toggle` classes have no CSS**
   — all three are hooks (JS/none needed): `.unlock-banner` is styled via
   `.alert.alert-warn` (identity.rs:101), `.theme-toggle` via `.btn-ghost`,
   `.nav-group-label` is an unstyled span inside a flex button. Dead-looking
   but intentional; do not "clean up" the hooks APP_JS queries
   (html.rs:34 `.unlock-banner`).

## G. Highest-leverage summary (what "looks bad" was)
1. Every page title renders twice + subtitles at 21px/600 (A1, A2) — ~30 pages.
2. Metric cards where the number is smaller than the label; four different
   metric idioms across four pages (B3.1, B11.2, B12.1, B1.2) → one `.stat`
   system (A7).
3. Settings accordions glued together with box-in-box nesting (A6).
4. A horizontal rail on phones with a vertical select column inside it (A10).
5. Unlock banner wider than the content column (A4).
6. Sea of primary-green row buttons (A12).
7. Analytics: filter above the title, default-blue Chart.js on paper-green,
   CDN script (B11).
8. Orphan destructive action on an empty cart; dead `href="#"` and `href="/"`
   links (B10.1, B14.2, B14.3).
9. Status vocabulary split: dot-badges on some pages, raw words on others
   (B15.1).
10. Duplicate element ids mis-binding form labels (B15.2, B15.3).

*Section counts: A = 12 items · B = 17 page groups / 44 discrete items ·
C = 8 findings + verified-OK list · D = 0 token changes, 2 record items ·
F = 6 flags.*
