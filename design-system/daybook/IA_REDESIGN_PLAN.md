# Web Console IA Redesign — Implementation Plan

**Spec:** `design-system/daybook/IA_REDESIGN_SPEC.md` (committed as `d03525f`)
**Implementation owner:** the web layer — all source edits are in web territory.
**Verification:** mandatory before any completion claim.
**Sign-offs:** design coherence before merge; owner AA-vs-AAA wording before ship.
**Status:** not started. No code has been changed in this session.

## Constraints (from the spec)

- Every URL path in the spec's §2 stays byte-identical; `src/web/mod.rs` route table is **untouched**.
- No new page gate, no schema/DB change, no env change.
- All links keep going through `state.url()` (base-path safe); skip link stays.
- Documentation-drift fixes are prose-only and listed as step 5 (web-owned trees).

## Step 1 — Add the declarative nav table (`src/web/html.rs`)

1. Define a `NavItem` struct with: `id: &'static str` (stable, never displayed), `label: &'static str`, `path: &'static str`, `group: &'static str`, `roles: Role` (a small enum or predicate set), and `titles: &'static [&'static str]` (the exact `title` strings existing pages pass to `html_page`).
2. Add one `const NAV_ITEMS: &[NavItem]` containing exactly the 27 group rows of spec §2 in render order, with groups in the order: Overview, Offerings, Sales, Fulfill, Communicate, Workspace. The three Utilities items (Settings, Theme, Sign out) are rendered separately per step 2.5 — do not include them in the table count.
3. Role constants derived from existing predicates only: `all`, `owner`, `seller_admin_owner` (mirrors `is_admin_role` at `src/web/mod.rs:565-567` plus seller). No new role enum or gate logic.
4. Add `fn active_nav_id(title: &str) -> Option<&'static str>` that finds the item whose `titles` contains `title`. This centralizes the title→id mapping in one place and replaces the scattered `title.eq_ignore_ascii_case(label)` comparisons. Pages whose title has no nav entry (e.g. "Restarting", "Add bot") simply get no highlight — do not add aliases unless a test requires one.

**Why titles, not paths:** `html_page` is called from 40+ sites with only a `title` (`src/web/html.rs:188-242`); adding a path/id parameter to every call site would be a large, risky diff. The table keeps the call sites unchanged while making the mapping explicit and testable. The acceptance criterion "no `title.eq_ignore_ascii_case(label)` remains" is met.

## Step 2 — Project the table in `rail_nav` (`src/web/html.rs:291-497`)

1. Replace the six hardcoded group blocks and the two bare `nav_link` calls with a loop over `NAV_ITEMS`: filter by session role, group by `group`, render each group with the **existing** markup (`section.nav-group[data-nav-group]`, `button.nav-group-toggle[aria-expanded][aria-controls]`, `div.nav-group-panel#nav-panel-{group}`).
2. Set `data-current="true"` by comparing the active item's `id` (from `active_nav_id(title)`) with the group's item ids — not by title/label string.
3. Keep `nav_link`/`nav_links` helpers only if still useful; ensure the rendered anchor gets `aria-current="page"` for the active `id`.
4. Keep the existing APP_JS (`src/web/html.rs:65-119`) unchanged — it already handles localStorage group state, mobile auto-expand, and `inert`/`aria-hidden` on collapsed panels.
5. Move the footer block (`src/web/html.rs:225`: Settings / Theme / Sign out) into a Utilities strip rendered after the groups inside the rail's `<nav>` (or immediately after it), with **Settings gated to owner** (`settings.rs:84-85`) and Theme + Sign out for all authenticated sessions. Remove the old footer `<div class="foot">…</div>`.
6. Verify `base`-prefixed hrefs: build every path with `state.url(path)` or the existing `base` prefix; do not hardcode absolute paths.

## Step 3 — Sidebar-toggle wording (`src/web/settings.rs`)

1. Update only labels/help text in the management panel (`settings.rs:354-397`) so the five toggles (AI, Crypto, Admins, Sellers, Web users) describe items in the **Workspace** group.
2. Do **not** change `SidebarPrefs`, `effective_sidebar_prefs`, `settings_sidebar`, storage keys, or the `/settings/sidebar` route.

## Step 4 — Tests (`src/web/tests/**`, `src/web/html.rs` inline tests)

1. Add unit tests for the table: every group/item/path/role matches spec §2; ids are unique; every `NAV_ITEMS` path resolves to a registered route (a grep-based or static assertion is fine); `active_nav_id` maps the known page titles.
2. Add role-visibility smoke tests: owner sees all items incl. Settings; seller/admin sees Finance/API keys/Abandoned carts but not owner-only items; buyer sees exactly the list in spec §10 and no Settings.
3. Add markup tests: group toggles carry `aria-expanded`/`aria-controls`; active anchor carries `aria-current="page"`; `data-current` set on the right group; skip link present; footer Settings link gone.
4. Update label-based assertions to new labels/groups ("Operate"/"Sell"/"Insights"/"Manage" → new names; "Cart" → "My cart", "Carts" → "Abandoned carts"). Path-based assertions (`src/web/tests/bots.rs:633-664`) must remain unchanged.

## Step 5 — Documentation drift (routing corrected)

- Documentation changes are limited to `docs/**`. Both stale items are in web-owned territory
  (`src/web/**` and `design-system/**`), so **the web layer fixes both in the same PR**:
  - the comment at `src/web/tests/bots.rs:652-653` ("Bots rides in the Operate rail group");
  - the matching prose in `design-system/daybook/BOT_SWITCHING_UX_SPEC.md`.
- Both must say Bots now lives in **Offerings**. No separate documentation handoff is needed for this change.

## Step 6 — web self-check, then hand to verification

Web runs locally (records output; does not claim its own verification):

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
cargo build
git diff --stat -- src/web/mod.rs   # must be empty — proves paths unchanged
```

Then run **verification**, with the spec's §10 acceptance criteria and §11 test plan as the checklist. Verification runs the full gate set including `cargo audit`, `cargo deny check`, confirms no route changed, and records the result. No completion claim before verification signs off.

## Step 7 — Manual visual check

At 1440 / 1024 / 768 / 375 px: group order, Utilities visibility, current-group auto-expand on phone, keyboard tab order + Enter/Space toggles, `aria-current`, focus rings, skip link, and `prefers-reduced-motion`. Verification records results.

## Open items (not blocking implementation)

- **Floor: WCAG 2.2 AA** — ruled on 2026-11-18; recorded in `IA_REDESIGN_SPEC.md` §8. No AAA upgrade, no color changes.
- **Design:** coherence sign-off on the new group/label set before merge.

## Rollback

Single PR, no feature flag, no DB migration. Revert the nav projection change (`html.rs`, `settings.rs` labels, tests) in one commit.
