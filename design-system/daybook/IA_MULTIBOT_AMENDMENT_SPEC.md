# Multibot / Multi-Surface IA — Bounded Amendment Spec

**Status:** Proposed amendment — ready for web / server / core implementation after owner approval
**Date:** 2026-11-18
**Discipline:** design (owner of this spec); extends the approved `IA_REDESIGN_SPEC.md`
**Amends:** `design-system/daybook/IA_REDESIGN_SPEC.md` (approved six-group + Utilities console IA)
**Territory:** the console navigation projection, the rail scope control, and the moderation configuration surfaces. No existing page path changes; no new auth; no token values change.
**Sources of truth:** `DESIGN.md` §1 / §3 / §4 and `assets/app.css`; `IA_REDESIGN_SPEC.md`; `BOT_SWITCHING_UX_SPEC.md` (shipped selector semantics); the repository quality gates.
**Hard constraints:** the six groups and Utilities stay as approved; no seventh group; no per-product consoles; no new route for an existing page; WCAG 2.2 AA is the single accessibility floor; tokens are unchanged (Radix 12-step role semantics over the `DESIGN.md` §4 ledger-green values).

---

## 1. Context / problem

The approved IA in `IA_REDESIGN_SPEC.md` was designed for one product: a single Telegram shop bot with owner / admin / seller / buyer roles. Its rail groups pages by goal (Overview, Offerings, Sales, Fulfill, Communicate, Workspace) plus a Utilities strip, and its only product-shaped control is a **bot selector** in the rail (owner picks a bot; sellers and admins see a pinned label). That model cannot represent more than one kind of product.

The owner now wants a family of surfaces in the same platform: **Movie bot**, **AI shop bot**, **Admin Reply Bot**, a **public Web app**, **Automation**, an **AI service**, **File Share**, and an **AI filter / word-ban** capability. These are heterogeneous:

- Some are Telegram bots (shop, movie, reply) — selectable as a *bot scope* today.
- Some are non-bot surfaces (public Web app, AI service, File Share) — they have no bot id and cannot be expressed by a bot-only selector.
- Some are cross-cutting capabilities (Automation, moderation) — they apply across products rather than being a product of their own.

Fitting these into the current IA by adding a nav group per product would explode the rail, and giving each product its own console would fragment login and tenancy. The structural answer is a new concept — **product type** — carried as metadata on a generalized scope selector, plus a **cross-cutting moderation layer** rather than a "moderation bot." This document is a **bounded amendment**: the six groups, every existing path, every existing gate, and every token stay as approved. Only the selector is generalized, product type is added as metadata, three new capability surfaces are introduced, and capabilities are packaged so an install registers only what it ships.

---

## 2. Goals / non-goals

**Goals**

- Keep the six groups (Overview, Offerings, Sales, Fulfill, Communicate, Workspace) and the Utilities strip exactly as approved.
- Generalize the existing bot selector into a **product / scope selector with a type facet**, preserving its shipped behavior (owner picks, sellers / admins pinned, server-side rejection of forged values).
- Introduce **product type** as metadata: Telegram bot types and non-bot surfaces are described by data, not by new nav groups.
- Disclose per-type configuration progressively: basics first, type-specific settings behind an "Advanced" disclosure.
- Add a **cross-cutting moderation hierarchy** at three levels (owner floor, seller-wide Blocked words, per-bot override) with empty, error, and appeal states plus an allow-list.
- Package capabilities behind feature flags so a **movie-only install registers only the movie capability and refuses other bot types**; the default build keeps all existing paths. (Dependency/binary-size trimming is deferred — see §5.)
- Hold the WCAG 2.2 AA floor and the unchanged token contract across every added control and state.

**Non-goals**

- No seventh nav group; no product-named groups.
- No per-product console, no second login, no per-product ports — one console, one login, as today.
- No new route, path rename, or path removal for any existing page; the only new routes are the capability pages this amendment explicitly introduces (§6).
- No new role tier, no new auth model, no new session cookie or credential flow.
- No new color value, theme, or token; no change to the Radix role semantics in `DESIGN.md` §4.
- No dynamic `.so` / plugin loading (deferred non-goal, as recorded in `AGENT.md`).
- No end-user appeal interface in the web console — the end-user appeal flow is bot-side and belongs to server (§4).
- No migration of stored business data beyond the additive `bot_type` metadata field (§6, §11).

---

## 3. Scope selector with a type facet

### 3.1 Where it lives and what it replaces

The control keeps its shipped placement and form: in `<aside class="rail">`, directly under the brand block and above the `<nav>`, so scope is read first after identity on every shell page. It generalizes `bot_scope_control` / `bot_select` from `BOT_SWITCHING_UX_SPEC.md` without changing where or how scope is persisted.

- The owner's **Bot** selector becomes a **Product** selector. Its reset option, "All bots (primary)" (`value=""`), becomes **"All products"** with byte-identical widening semantics: unscoped global view, never a filter on the literal primary product.
- One option per enabled product — bots keyed by bot id, non-bot surfaces keyed by their own stable surface id — labeled by plain-language name plus, for bots, the `key · @username` form from the shipped spec (username omitted when unset).
- Disabled products are excluded from the options and rejected server-side, exactly as today.

### 3.2 The type facet

The selector gains a **type facet**: the product options are organized by product type, and a native `<select id="scope-type">` labeled "Type" offers "All types" plus each compiled-in product type. Selecting a type narrows the Product select to that type; this is a client-side aid only — it never submits, never persists, and never gates access, so with JavaScript unavailable every product is still present and grouped by type and no state is lost. The persisted state remains the single selected product id on the session row; the facet adds no new storage and no new permission.

Options are also grouped with `<optgroup>` labels derived from each product's `type` metadata, so the type is legible without opening the facet:

| Product type (metadata value) | Example surfaces | Grouped under |
|---|---|---|
| `shop_bot` | AI shop bot | Shop bots |
| `movie_bot` | Movie bot | Movie bots |
| `reply_bot` | Admin Reply Bot | Reply bots |
| `web_app` | public Web app | Web apps |
| `service` | AI service | Services |
| `file_share` | File Share | File sharing |
| `automation` | Automation | Automation |

### 3.3 Metadata model and progressive disclosure

Each selectable scope carries server-derived metadata: `id`, `label`, `type` (one of the values above), `enabled`, `key` / `username` for bots, and the set of capabilities it owns. The type facet and the nav's runtime type filter read `type` from this metadata; it is **not** a new nav group and not a new role.

Per-type configuration is **shared and progressively disclosed**: existing pages (notably `/bots`) gain a `product type` field and an **Advanced** disclosure (native `<details>` / `<summary>`), closed by default. The basic fields every product shares render first; type-specific fields (movie catalog, reply templates, file-share limits, automation triggers) appear only when the type calls for them and only after the seller opens Advanced. No new configuration route is introduced for these fields — they extend existing pages.

### 3.4 Owner / seller / buyer states

| Who | Variant | Behavior |
|---|---|---|
| Owner | Product selector + Type facet + visible **Switch** button | Native `<select>` submits on change as progressive enhancement; the visible button keeps keyboard and no-JS paths working. POST validates, persists against the session row, PRG-redirects to the console-local Referer path else `/dashboard`. |
| Seller / admin | Pinned label, `aria-label="Active product"` | Plain text naming the resolved product (bot key or surface name) via the same server-side resolution as today. Never styled as a disabled control, no fake-select chrome. A forged switch attempt is rejected server-side (403, `forbidden_page`); the UI never offers the action. |
| Buyer (User) | Absent | No scope control and no moderation configuration. The buyer's view is their own purchases and orders, as today. |

### 3.5 Mobile (375px)

At the existing 900px drawer breakpoint the control stacks: "Type" label above the type select, "Product" label above the product select, and a full-width Switch button — all ≥44px tall, no horizontal scroll, no side-by-side selects. `<optgroup>` headings and the pinned label wrap onto two lines rather than truncating. The current group still auto-expands and Utilities stay pinned, per `IA_REDESIGN_SPEC.md` §5.

---

## 4. Moderation hierarchy — a cross-cutting layer, not a standalone bot

Moderation is a **cross-cutting configuration layer** applied to products that carry messages. It has no product type, no selector entry, and no standalone nav group. Three levels stack, from least overridable to most local:

| Level | Surface | Group | Roles | Precedence |
|---|---|---|---|---|
| (a) Owner / global policy | "Content policy" page, beside **AI** in Workspace | Workspace | Owner only | Non-overridable floor |
| (b) Seller-wide rules | "Blocked words" page | Offerings | Seller / admin / owner (scoped) | Adds to the floor; may be stricter, never looser |
| (c) Per-bot override | "Advanced" disclosure on `/bots` | Offerings (via `/bots`) | Owner-managed bot metadata; seller rules inherited | Inherits (b) by default; may add stricter local rules, never below (a) |

**Modes** are the same plain-language three at every level: **hide** (matching messages are hidden), **warn** (a warning is shown), **review** (matching messages are sent for review). Each mode is labelled with a one-line plain-language description; no jargon such as "AI filter pipeline."

### 4.1 (a) Owner / global policy — non-overridable floor

A Workspace item beside **AI**, owner-only, holding the global baseline: the minimum enforcement mode and the baseline banned-term set. It is a **floor**: seller and per-bot rules may add terms or raise enforcement, but may never loosen a baseline rule or drop below the floor mode. The page states this in plain language near the top. It carries the same one-primary-action rule as the seller page.

### 4.2 (b) Seller-wide "Blocked words" — the primary surface

Named in plain language ("Blocked words" — not "Content filters" or "AI filter configuration"). The page has **one primary action, "Add a word"**, plus a simple list of words with their mode and a status. Mode changes are inline per row (native select), not a second competing CTA. Copy is written for non-technical sellers.

### 4.3 (c) Per-bot override in `/bots` Advanced

Inside the existing `/bots` Edit form's Advanced disclosure, a per-bot moderation override defaults to **"Using your shop's rules"** — meaning the bot inherits its seller's Blocked words. Choosing "Custom rules for this bot" reveals a word list and mode select scoped to that `bot_id` and its `seller_id`; these can add to but not weaken the seller and global layers. Returning the override to "Using your shop's rules" restores inheritance.

### 4.4 States

- **Empty state:** "No blocked words yet. Add a word to hide it, warn on it, or send matching messages for review." with the "Add a word" action as the only CTA. The per-bot override empty state says "Using your shop's rules."
- **Error state:** inline validation next to the field for empty input, duplicates, and over-long terms; on save failure the entered text and chosen mode are preserved and a **Retry** action is offered. The seller's input is never discarded on an error.
- **Appeal state:** appealed items appear in a review list with a **Request review** action, a reason field, and a status shown as a dot-before-word badge (Pending / Approved / Upheld). Status is conveyed in text, never by color alone.
- **Allow-list for false positives:** a separate "Always allow" list on the seller page holds terms exempt from filtering; it takes precedence over Blocked words for that seller while remaining above the global floor. It is managed with the same one-primary-action pattern ("Add an allowed word").

**Boundary:** the end-user appeal — a buyer contesting a hidden or warned message in the bot — is **bot-side and belongs to the server layer** (`src/handlers/**`, dialogue). The web console surfaces only the seller / owner review queue and its status; it does not implement the buyer-facing appeal conversation. Any console surface that would expose appeal message content or user presence is routed to security first (§11).

---

## 5. Feature-gated capability packaging

Capabilities are selected at compile time and gate **registration, navigation visibility, and which bot types are accepted at runtime** — they do not trim dependencies or binary size:

- Each nav item in the declarative table carries an optional `feature: &'static str` and an optional `product_types` set. `rail_nav` filters items by compile-time `#[cfg(feature = "...")]` and, at runtime, by the selected product's `type` metadata.
- A **movie-only install** enables the `movie_bot` capability and **registers** only that capability's nav item, config fields, and handler module; other types are absent from nav/type facet and bots of those types are explicitly refused (`capability_uncompiled`). **All Cargo dependencies remain compiled and linked** (`cargo tree --depth 1` identical to default; binary not smaller); omission requires web+server module gating then core `Cargo.toml` optional/dep mappings — a deferred cross-team follow-up outside this foundation's scope.
- The **default build** keeps every path in `IA_REDESIGN_SPEC.md` §2 registered and byte-identical. Feature gating changes nav visibility and which capability modules are registered — it never renames, moves, or removes an existing path, and it introduces no new route for an existing page.
- The runtime type filter is a view filter only: hiding an item for a product type does not change a page gate or a write authorization. This mirrors the shipped selector rule that scoping narrows what is read, not what is permitted.
- Capability pages introduced by this amendment (Blocked words, Content policy, Automation) follow the same rule: their own routes are registered only when their capability is compiled in; no existing route is altered.

---

## 6. Classification table

### Absorbed by the current IA (no new group, no path change)

| Surface / capability | Handling |
|---|---|
| Movie bot | New `product_type` metadata value; selectable in the generalized selector. No nav group, no new route. |
| AI shop bot | `product_type` metadata; today's primary bot behavior retained. No nav group. |
| Admin Reply Bot | `product_type` metadata; selectable like other bots. No nav group. |
| Public Web app | Non-bot surface metadata; reached at its own public URL if/when that capability is built, not as a console group. |
| AI service | Metadata; its configuration is the existing Workspace **AI** item. No new group or route. |
| File Share | Metadata; delivered as a product type under Offerings (Products). No new group. |
| Buyer view / My cart | Unchanged from `IA_REDESIGN_SPEC.md` §2. |
| Per-type configuration | Progressive disclosure inside existing pages (e.g. `/bots` Advanced). No new route for existing pages. |

### Requires this amendment

| New surface / change | Handling |
|---|---|
| Seller "Blocked words" page | New seller-wide page in Offerings; plain language, one "Add a word" action, hide / warn / review modes, empty / error / appeal states, allow-list (§4.2, §4.4). |
| Owner "Content policy" page | New Workspace item beside **AI**; non-overridable global floor (§4.1). |
| Generalized selector + `bot_type` field | Selector gains the type facet and product metadata; an additive `bot_type` field is introduced by core (§3, §11). |
| Automation | Promoted to a first-class nav item in Workspace with its own capability page, gated by feature flag; no seventh group (§5). |

---

## 7. Role / tenancy rules

- **Role tiers are reused.** The nav table keeps the existing tiers — `all`, `owner`, `seller_admin_owner` — and the same page gates as `IA_REDESIGN_SPEC.md` §6. No new role enum, no new gate on any existing page. The three new pages carry their own gates: Content policy owner-only; Blocked words seller / admin / owner; Automation per its capability gate and role set.
- **Tenancy.** Every business record added here — blocked words, allow-list entries, per-bot overrides, appeal queue items, automation rules — is scoped by `seller_id`. Per-bot overrides are scoped by both `bot_id` and `seller_id`. An owner sees the global floor plus all sellers through the existing global visibility; a seller or admin sees only their own seller's records.
- **Selector state.** Scope persists only as the selected product id on the session row, as shipped. The type facet is client-side and stores nothing. Forged product ids, type values, or seller ids are rejected server-side (403, `forbidden_page`); the selector only narrows reads, never widens permission.
- **No new auth.** No new cookie, token, login surface, or session credential is introduced. Buyers see no scope control and no moderation configuration. This preserves the multi-tenant and owner/admin-gating invariants in `AGENT.md`.

---

## 8. Accessibility invariants

- **WCAG 2.2 AA is the single release floor**, non-waivable, across the selector, facet, disclosures, moderation pages, and every empty / error / appeal state.
- **Active state is id-keyed.** The nav highlight uses the stable nav item `id`, not a page title; no `title.eq_ignore_ascii_case(label)` comparison remains. The active link carries `aria-current="page"`.
- **ARIA and semantics.** The Product and Type selects are native controls with programmatic `<label for>`; `<optgroup>` labels name each type. The pinned label uses `aria-label="Active product"`. Advanced uses native `<details>` / `<summary>`. Mode selects and reason fields are labeled. Scope-change feedback uses a `role="status"` live region.
- **Skip link.** `<a class="skip-link" href="#main">Skip to content</a>` stays first in the document and is unchanged.
- **Keyboard and focus.** Every control is tab-reachable in visual order; focus rings use the house `:focus-visible`; no `outline: none`. The visible Switch button guarantees a non-trapping keyboard and no-JS path.
- **Target size and motion.** All touch targets, including the facet, selects, Switch, "Add a word," and "Request review," are ≥44px at 375px. Chevron and disclosure motion respects `prefers-reduced-motion` and is CSS-only.
- **One primary action per screen.** Each surface has a single CTA (Switch; "Add a word"; "Add an allowed word"); mode and status changes are secondary inline controls. Progressive disclosure keeps Advanced closed by default.
- **Unchanged tokens.** All color and typography come from `DESIGN.md` §4 / `assets/app.css` using the Radix 12-step role semantics; no new color value is introduced. Status is a dot before a word plus text, never color alone. Text pairs remain ≥4.5:1 in both themes.

---

## 9. Acceptance criteria

- [ ] The six groups and Utilities render in the approved order; no seventh group and no product-named group appears.
- [ ] No existing page path, gate, or route is changed; the only new routes are the capability pages in §6.
- [ ] The rail scope control is a Product selector with a Type facet; "All products" retains the widening semantics of "All bots (primary)."
- [ ] Owner sees the selector + facet + visible Switch; seller / admin see a pinned "Active product" label; buyer sees no scope control.
- [ ] A forged product id, type value, or seller id is rejected server-side (403); scope narrows reads only.
- [ ] Product type is metadata on the selector; bot types and non-bot surfaces are represented without new nav groups.
- [ ] Per-type configuration is progressively disclosed behind a closed-by-default Advanced disclosure on existing pages, with no new route for those fields.
- [ ] A movie-only feature build registers only the movie capability and refuses other bot types; the default build keeps every existing path registered. Dependency sets are intentionally unchanged (§5).
- [ ] Moderation is a cross-cutting layer with no standalone bot: owner Content policy (non-overridable floor), seller Blocked words (one "Add a word" action, hide / warn / review), per-bot override defaulting to "Using your shop's rules."
- [ ] Blocked words has an empty state, inline validation with retry that preserves input, an appeal review list with "Request review" + reason + status, and an "Always allow" allow-list taking precedence over blocked words while staying above the floor.
- [ ] Seller / bot / appeal records are scoped by `seller_id` (and `bot_id` for overrides); no new auth, cookie, or role tier.
- [ ] Active nav state is id-keyed with `aria-current="page"`; no title-based comparison remains; skip link retained.
- [ ] Native labeled selects, `aria-label` pinned label, `<details>` semantics, `role="status"` feedback, visible focus, ≥44px targets, reduced motion, and one primary action per screen verified at 375 / 768 / 1024 / 1440px.
- [ ] No new token or color value; all pairs from `DESIGN.md` §4 verified ≥4.5:1 in both themes.
- [ ] `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo audit`, `cargo deny check` pass; verification recorded before completion.

---

## 10. Risks / rollout

- **Rail complexity / seventh-group pressure.** Adding many surfaces tempts one-group-per-product. Mitigated by the type-facet metadata model (§3) and the explicit no-seventh-group non-goal; the rail item count stays fixed.
- **Scope creep into auth and tenancy.** The selector must remain a read-filter. Mitigated by reusing the session-row persistence and 403 rejection from `BOT_SWITCHING_UX_SPEC.md`; security reviews any change that touches scope validation.
- **Moderation false positives and abuse.** Blocked words can over-match. Mitigated by the "Always allow" allow-list, the three explicit modes, the appeal review state, and the non-overridable owner floor.
- **Feature-flag drift.** A nav item could appear for a capability not compiled, or a path could change in a gated build. Mitigated by tying nav items to `cfg(feature)` and the type set, and by a verification check that the default build's route table is byte-identical.
- **Accessibility regression.** New controls and states can break the floor. Mitigated by §8 invariants and verification at four viewports including 375px.
- **Rollout.** Ship as a bounded amendment in team-owned changes: web (nav + selector + capability pages), server (bot-side appeal), core (`bot_type` field), then verification. Capability pages are feature-gated and opt-in; the default build is behavior-compatible with the approved IA. Rollback is a revert of each team's change; no data migration to unwind beyond the additive `bot_type` field.

---

## 11. Ownership

- **Spec owner:** this document.
- **Web:** nav projection, scope selector + type facet, capability-gated nav items, and the three web configuration surfaces (Blocked words, Content policy, Automation) in `src/web/**`.
- **Server:** the bot-side end-user appeal flow and any handler / dialogue work that feeds the appeal queue (`src/handlers/**`, `src/dialogue.rs`).
- **Core:** the additive `bot_type` field and any data-layer / config support in `src/db.rs`, `src/config.rs`, or `Cargo.toml` for feature packaging.
- **Verification:** mandatory before any completion claim — full gate set, §9 acceptance criteria, and confirmation that no existing path or gate changed.
- **Security:** engaged on any content-policy capability that would expose message content or user presence; a security finding is a floor, not a peer, and escalation follows the conflict rules.
- **Design coherence sign-off:** not triggered — no token value or brand token changes in this amendment.
