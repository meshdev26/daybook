# Web Console Information Architecture Redesign — Goal-Based Grouping, Six Groups + Utilities

**Status:** Approved, ready for L7 implementation
**Date:** 2026-11-18
**Role:** L6 senior-design-ux (owner of this spec); L6-C sign-off required on token/brand coherence
**Territory of this spec:** the console navigation only — `src/web/html.rs` (`rail_nav`, `nav_link`, `nav_links`, the `html_page` footer), plus a documentation note on the sidebar-toggle remap in `src/web/settings.rs`. No route, schema, or DB changes; no new auth logic; no new pages.
**Sources of truth:** `DESIGN.md` + `assets/app.css` (MASTER.md is deprecated 2026-10-31 and must not be used).
**Hard constraints:** every URL path stays byte-identical (no route changes); every page gate stays as-is (no new auth logic); no DB or schema changes; all existing hrefs/actions continue to go through `state.url()` (base-path safe); the skip link stays.

---

## 0. Owner mandate and the structural answer

Today the console rail is organized around *implementation surfaces* (Operate, Identity, Sell, Fulfill, Insights, Manage) plus two bare links (API keys, Carts) and a Settings link buried in the footer. Labels and placement reflect how the code is split, not how a seller thinks:

- Two cart pages share near-identical nav labels ("Cart" vs "Carts") with no audience cue (`/cart` = buyer's own basket, `src/web/cart.rs:19`; `/carts` = seller/admin abandoned-carts report, `src/web/cart.rs:149`).
- Finance sits in *Sell* next to Codes/Promos, even though it is owner money ops beside Crypto.
- Analytics sits in *Insights* next to Posts/Broadcast, even though it is read-only insight, not authoring.
- Settings is reachable only through the footer (`src/web/html.rs:225`), a discoverability hole.
- Active state is brittle: `title.eq_ignore_ascii_case(label)` at `src/web/html.rs:277`, so a page whose title drifts from its nav label silently loses its highlight.
- Owner sidebar toggles (AI/Crypto/Admins/Sellers/Web users) live in `src/web/settings.rs:5-47` and `/settings/sidebar`, decoupled from the nav they configure.

The answer is a **relabel and regroup**, not a rebuild: the same routes, the same pages, the same gates — six goal-based groups plus a persistent Utilities strip, with a declarative nav table and id-keyed active state.

---

## 1. Verified current state (file:line)

| Fact | Location |
|---|---|
| `rail_nav` (hardcoded groups) | `src/web/html.rs:291-497` |
| `nav_link` (brittle active state) | `src/web/html.rs:276-283` (`title.eq_ignore_ascii_case(label)`) |
| `nav_links` helper | `src/web/html.rs:285-289` |
| `html_page` footer (Settings link) | `src/web/html.rs:225` |
| `html_page` skip link | `src/web/html.rs:219` |
| Routes registered | `src/web/mod.rs:229-403` |
| Buyer cart page | `src/web/cart.rs:19` (`cart_page`, title "Cart") |
| Seller/admin abandoned-carts page | `src/web/cart.rs:149` (`carts_page`, title "Carts") |
| Sidebar prefs struct + toggles | `src/web/settings.rs:4-47`, `/settings/sidebar` (`settings_sidebar`) |
| Owner sidebar management panel | `src/web/settings.rs:354-397` |
| AA-vs-AAA wording drift | `AUTO_ROUTE.md:102` (AA) and `.opencode/agents/senior-design-ux.md:34` (AA) vs. `aaa-gate` skill and `level-router` skill (AAA) |

Current groups and items (verbatim, in render order):

- **Operate** (all roles render the group; Bots owner-only): Dashboard, Channels, Plans, Products, Bots[owner].
- **Identity** (owner only): Identity, Monitor.
- **Sell** (all roles): Codes, Promos, Referrals, Orders, Finance, Customers, Cart. Bare links after: API keys (seller/admin), Carts (seller/admin).
- **Fulfill** (all roles): Subscriptions, Bookings, Tickets.
- **Insights** (all roles): Analytics, Posts, Broadcast.
- **Manage** (owner only, pref-gated): AI, Crypto, Admins, Sellers, Web users.

---

## 2. Approved IA — nav map (paths unchanged)

| Group | Item (new label) | Path | Roles (reuses existing page gates) |
|---|---|---|---|
| Overview | Dashboard | `/dashboard` | all authenticated |
| Overview | Analytics | `/analytics` | all authenticated |
| Overview | Monitor | `/monitor` | owner |
| Offerings | Products | `/products` | all authenticated |
| Offerings | Plans | `/plans` | all authenticated |
| Offerings | Channels | `/channels` | all authenticated (page scopes by role) |
| Offerings | Bots | `/bots` | owner |
| Sales | Orders | `/orders` | all authenticated (page scopes by role) |
| Sales | Customers | `/customers` | all authenticated (page scopes by role) |
| Sales | Codes | `/codes` | all authenticated (page scopes by role) |
| Sales | Promos | `/promos` | all authenticated (page scopes by role) |
| Sales | Referrals | `/referrals` | all authenticated (page scopes by role) |
| Sales | My cart | `/cart` | all authenticated (buyer's own basket) |
| Sales | Abandoned carts | `/carts` | seller / admin / owner |
| Fulfill | Subscriptions | `/subs` | all authenticated (management gated to owner on the page) |
| Fulfill | Bookings | `/bookings` | all authenticated (page scopes by role) |
| Fulfill | Tickets | `/tickets` | all authenticated (page scopes by role) |
| Communicate | Posts | `/posts` | all authenticated (page scopes by role) |
| Communicate | Broadcast | `/broadcast` | all authenticated (page scopes by role) |
| Workspace | Identity | `/identity` | owner |
| Workspace | AI | `/ai` | owner |
| Workspace | Crypto | `/crypto` | owner |
| Workspace | Finance | `/finance` | seller / admin / owner |
| Workspace | Admins | `/admins` | owner |
| Workspace | Sellers | `/sellers` | owner |
| Workspace | Web users | `/webusers` | owner |
| Workspace | API keys | `/keys` | seller / admin / owner |
| Utilities (bottom, always visible) | Settings | `/settings` | owner (page-gated; non-owners get the forbidden page) |
| Utilities (bottom, always visible) | Theme | (client toggle) | all authenticated |
| Utilities (bottom, always visible) | Sign out | `/logout` | all authenticated |

No path is added, removed, or renamed. The only label changes are "Cart" → "My cart", "Carts" → "Abandoned carts", and the group labels.

**Correction (self-review):** the earlier "7-item reduced set" claim was a mistake and is removed. No new nav-level role filtering is introduced by this redesign. A plain **User (buyer)** account sees the same items it sees today, minus the owner-only and seller/admin-only ones — i.e. Dashboard, Channels, Plans, Products, Codes, Promos, Referrals, Orders, Customers, My cart, Subscriptions, Bookings, Tickets, Analytics, Posts, Broadcast. Settings is owner-only (`src/web/settings.rs:84-85` returns `forbidden_page` for non-owners), so it is omitted from Utilities for buyers; Theme and Sign out remain available to all authenticated users. The nav simply filters items by the same gates the pages already enforce; no new role check is introduced.

---

## 3. Before/after mapping of every current item

| Current item | Current group | New group | New label | Path |
|---|---|---|---|---|
| Dashboard | Operate | Overview | Dashboard | `/dashboard` |
| Channels | Operate | Offerings | Channels | `/channels` |
| Plans | Operate | Offerings | Plans | `/plans` |
| Products | Operate | Offerings | Products | `/products` |
| Bots | Operate | Offerings | Bots | `/bots` |
| Identity | Identity | Workspace | Identity | `/identity` |
| Monitor | Identity | Overview | Monitor | `/monitor` |
| Codes | Sell | Sales | Codes | `/codes` |
| Promos | Sell | Sales | Promos | `/promos` |
| Referrals | Sell | Sales | Referrals | `/referrals` |
| Orders | Sell | Sales | Orders | `/orders` |
| Finance | Sell | Workspace | Finance | `/finance` |
| Customers | Sell | Sales | Customers | `/customers` |
| Cart | Sell | Sales | My cart | `/cart` |
| API keys (bare link) | (bare) | Workspace | API keys | `/keys` |
| Carts (bare link) | (bare) | Sales | Abandoned carts | `/carts` |
| Subscriptions | Fulfill | Fulfill | Subscriptions | `/subs` |
| Bookings | Fulfill | Fulfill | Bookings | `/bookings` |
| Tickets | Fulfill | Fulfill | Tickets | `/tickets` |
| Analytics | Insights | Overview | Analytics | `/analytics` |
| Posts | Insights | Communicate | Posts | `/posts` |
| Broadcast | Insights | Communicate | Broadcast | `/broadcast` |
| AI | Manage | Workspace | AI | `/ai` |
| Crypto | Manage | Workspace | Crypto | `/crypto` |
| Admins | Manage | Workspace | Admins | `/admins` |
| Sellers | Manage | Workspace | Sellers | `/sellers` |
| Web users | Manage | Workspace | Web users | `/webusers` |
| Settings | (footer) | Utilities | Settings | `/settings` |
| Theme | (footer button) | Utilities | Theme | (client toggle) |
| Sign out | (footer/management panel) | Utilities | Sign out | `/logout` |

Every current item is accounted for; no item is dropped, no item is added.

---

## 4. Rationale for each move

- **Analytics → Overview.** Analytics is read-only insight about the business, not authoring. Grouping it with Dashboard and Monitor (the other "look at the numbers" surfaces) keeps authoring surfaces (Posts, Broadcast) together in Communicate.
- **Posts + Broadcast → Communicate.** Both are authoring/sending actions. They were split from Analytics so that "read" and "write" surfaces no longer share a group.
- **Finance → Workspace, beside Crypto.** Finance is owner money ops (payouts, refunds, reconciliation) and shares ownership with Crypto (the payment rail). It is not a promo tool and never belonged in Sell next to Codes/Promos.
- **Cart / Carts renamed by audience.** `/cart` is the buyer's own basket → "My cart"; `/carts` is the seller/admin abandoned-carts report → "Abandoned carts". The audience cue removes the ambiguity that two near-identical labels created.
- **Settings promoted from footer to Utilities.** The footer link (`src/web/html.rs:225`) was the only entry point and was easy to miss; promoting it to an always-visible Utilities strip makes account/console settings discoverable on every screen.
- **Bare links eliminated.** API keys and Carts were rendered as loose links outside any group (`src/web/html.rs:470-475`). They now live inside Workspace and Sales respectively, so every item has a home and a consistent affordance.
- **Bots → Offerings.** Bots are a posting target alongside Channels/Products/Plans, not an operate-level switch. (Documentation drift: `src/web/tests/bots.rs:652-653` and `BOT_SWITCHING_UX_SPEC.md` prose say "Bots rides in the Operate rail group for the owner only" — both become stale and must be updated by L4 during implementation. The test itself only asserts `href="/bots"` presence/absence and survives; only the comment drifts.)

---

## 5. Navigation mechanics (L7 implements in `src/web/html.rs`)

1. **One declarative NavItem table.** Replace the scattered `nav_link`/`nav_links` calls in `rail_nav` with a single table of structs, each carrying at minimum: `id` (stable, never a label), `label`, `path`, `group`, `roles` (the role set that may see it). `rail_nav` becomes a pure projection of that table filtered by the session's role. No second source of truth.
2. **Active state keys off `id`, not the page title.** The handler passes the page's nav `id` (or the router resolves it from the path) instead of a display title; `nav_link`'s `title.eq_ignore_ascii_case(label)` comparison at `src/web/html.rs:277` is removed. This closes the silent-loss-of-highlight bug.
3. **Groups stay collapsible with correct ARIA.** Each `.nav-group` keeps `data-nav-group`, the toggle button keeps `aria-expanded` and `aria-controls="nav-panel-{group}"`, and the panel keeps `id="nav-panel-{group}"`. The existing APP_JS in `src/web/html.rs:65-119` already implements this correctly (localStorage persistence, mobile auto-expand of the current group, `inert`/`aria-hidden` on collapsed panels) — it is reused unchanged.
4. **Current group auto-expands on phones.** Keep the existing rule: on mobile (`max-width: 900px`), a group with `data-current="true"` defaults open when no stored preference exists. With id-keyed active state, `data-current` is set by comparing the active item's `id` to the group's items, not by title string matching.
5. **Skip link stays.** `<a class="skip-link" href="#main">Skip to content</a>` at `src/web/html.rs:219` is unchanged.
6. **Utilities are rendered outside the collapsible groups**, always visible at the bottom of the rail, replacing the old footer block at `src/web/html.rs:225`. The **Settings link is owner-only** (`src/web/settings.rs:84-85`); Theme (toggle button) and Sign out (`/logout`) are available to all authenticated users. Theme keeps `toggleTheme()`; Sign out keeps `/logout`.
7. **Mobile-first, one primary action per screen, progressive disclosure.** The rail already collapses to a drawer on phones (CSS breakpoint 900px); no new interaction model is introduced. Group order on mobile follows the table order above; Utilities remain pinned.
8. **No auth logic changes.** Role filtering in the nav uses the same role strings and `is_admin_role` helper already defined in `src/web/mod.rs:562-567`; no new gates.

---

## 6. Role visibility (reuses existing page gates)

The nav must never show a link whose page would return `forbidden_page`. Filtering is by the role sets in §2, which are derived from the existing handlers (e.g. `bots_page` owner-only at `src/web/bots_admin.rs:388-389`; `crypto_page` owner-only at `src/web/admin_console.rs:276`; `keys_page` seller/admin/owner at `src/web/mod.rs:351` and `is_admin_role`; `finance_page` seller/admin/owner at `src/web/finance.rs:213`; `settings_page` owner-only at `src/web/settings.rs:84-85`). Settings is the concrete case the owner-only filter enforces: it appears in Utilities only for owners. L7 must not add or remove a gate; if a page's gate changes later, the nav table's `roles` field is updated in the same change.

---

## 7. Migration

1. **No route, schema, or DB changes.** All paths in §2 already exist in `src/web/mod.rs:229-403`. No migration of stored data.
2. **Sidebar toggles remap to Workspace.** The five owner toggles in `src/web/settings.rs:5-47` (ai, crypto, admins, sellers, webusers) and the management panel at `src/web/settings.rs:354-397` keep their storage keys and POST route (`/settings/sidebar`); only their *grouping* changes — they now control visibility inside the Workspace group instead of a standalone Manage group. The `SidebarPrefs` struct, `effective_sidebar_prefs`, and `settings_sidebar` handler are unchanged in shape; only labels/help text may be updated to say "Workspace". No DB key migration.
3. **Label-based nav tests must be updated; path-based tests untouched.** Any test that asserts a nav group label string (e.g. "Operate", "Sell", "Insights", "Manage", "Identity") or a nav item label ("Cart", "Carts", "API keys" as bare link) needs its expected string updated to the new labels/groups. Tests that assert only paths, hrefs, or page presence (e.g. `src/web/tests/bots.rs:633-664` asserts `href="/bots"` presence/absence) survive unchanged. Specifically:
   - `src/web/tests/bots.rs:633-664` (`shell_shows_bot_selector_for_owner_and_pinned_label_for_seller`) survives on path; its comment at `:652-653` becomes stale and must be corrected by L4.
   - `src/web/tests/subs.rs:17` (asserts body contains "Subscriptions") survives — label unchanged.
   - `design-system/telegram-agent/BOT_SWITCHING_UX_SPEC.md` prose referencing Bots in the Operate group becomes stale and must be updated by L4 during implementation.
4. **No data migration, no env changes.** `WEB_BASE_PATH`, cookies, and redirects are unaffected.

---

## 8. Interaction & accessibility requirements

- **Keyboard.** Every group toggle is a real `<button>` with `aria-expanded`/`aria-controls`; Enter/Space toggles; Tab order follows visual order (brand → bot scope → nav groups → Utilities). Focus rings use the house `:focus-visible` (`assets/app.css:154`).
- **Screen reader.** Group labels are announced as button names; collapsed panels are `inert` + `aria-hidden="true"` (already implemented in APP_JS); the active page link carries `aria-current="page"` (keyed by id). Skip link first.
- **Mobile (375px).** Groups stack; the current group is the only one open by default on a phone; Utilities remain reachable (sticky bottom of the drawer). Touch targets ≥ 44px (house `.btn`/toggle padding).
- **Reduced motion.** Respect `prefers-reduced-motion` for the chevron rotation (CSS-only, no JS animation added).
- **Contrast.** All text uses token pairs from `DESIGN.md` §4 / `assets/app.css`; no new color values.

### Open decision (explicit, for L11/human — not resolved here)

The repository carries two different accessibility floors:

- `AUTO_ROUTE.md:102` and `.opencode/agents/senior-design-ux.md:34` state **WCAG 2.1 AA**.
- The `aaa-gate` skill and the `level-router` skill state **WCAG AAA** as the release floor.

This spec does not pick between them. L11/human must reconcile the wording before the nav change ships, so that the implemented nav is verified against a single, agreed floor. Until then, L7 implements to the stricter of the two (AAA) for any new markup, and the wording is left to be corrected in a separate, explicit change. No silent downgrade from AAA to AA is permitted.

---

## 9. Ownership & verification

- **Spec owner:** L6 (this document).
- **Design-system / token / brand coherence sign-off:** L6-C (Chief Design Officer), because this changes cross-surface grouping and labels.
- **Implementation owner:** L7 (Staff Client Platforms), in `src/web/html.rs`. `src/web/settings.rs` receives label/help-text edits only.
- **Verification:** L3 (Associate Verification) — mandatory before completion. L3 runs the full gate set and the test plan in §11 and confirms no path changed.

---

## 10. Acceptance criteria

- [ ] Rail renders the six groups (Overview, Offerings, Sales, Fulfill, Communicate, Workspace) plus a Utilities strip, in that order, with the item lists in §2.
- [ ] Every URL path in §2 is unchanged; no new route is added or removed.
- [ ] No new auth logic; role filtering matches the existing page gates in §6.
- [ ] Active page is highlighted by stable `id`, not by page title; no `title.eq_ignore_ascii_case(label)` remains in nav code.
- [ ] A single declarative NavItem table is the only source of nav items.
- [ ] Collapsible groups retain `aria-expanded`, `aria-controls`, `data-nav-group`, and `inert`/`aria-hidden` behavior; skip link retained.
- [ ] Settings is reachable from Utilities on owner sessions only; Theme and Sign out remain in Utilities for all authenticated users.
- [ ] A buyer (User) session sees exactly: Dashboard, Channels, Plans, Products, Codes, Promos, Referrals, Orders, Customers, My cart, Subscriptions, Bookings, Tickets, Analytics, Posts, Broadcast — and does NOT see Settings, Monitor, Bots, Identity, AI, Crypto, Finance, Admins, Sellers, Web users, API keys, or Abandoned carts.
- [ ] Settings appears in Utilities for owner sessions only (owner-only page gate, `src/web/settings.rs:84-85`).
- [ ] Owner sidebar toggles still function and now describe Workspace items.
- [ ] `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` pass.
- [ ] L3 verification recorded before completion.

---

## 11. Test plan

**Viewports (manual + automated where available):** desktop 1440, desktop 1024, tablet 768, phone 375. Verify group order, Utilities visibility, and that the current group auto-expands on phones.

**Keyboard:** Tab through the rail; Enter/Space toggles groups; active link has `aria-current="page"`; focus rings visible; skip link works.

**Screen reader (manual):** group toggle announces label + expanded state; collapsed panel is not announced; active page announced; Utilities items announced.

**Automated:** update label-based nav assertions to the new labels/groups; path-based assertions (e.g. `src/web/tests/bots.rs:633-664`) remain unchanged; add a smoke check that each role sees exactly its expected item set. L3 verifies the full suite.

---

## 12. Risks & rollout

- **Label drift in tests.** Mitigated by §7.3: L7 updates label-based assertions in the same change; L3 confirms path-based ones are untouched.
- **Documentation drift.** `src/web/tests/bots.rs:652-653` comment and `BOT_SWITCHING_UX_SPEC.md` prose become stale (Bots no longer in Operate). Recorded here as an L4 fix item during implementation; not blocking.
- **Active-state regression.** Mitigated by id-keyed state and L3 verification of highlight on every page.
- **Accessibility floor ambiguity.** Mitigated by the open decision in §8, escalated to L11/human; no silent downgrade.
- **Rollout.** Single PR, no feature flag, no DB migration. Rollback is a revert of the nav projection change.
