# Security Review — Web Bot Scoping + Sidebar Bot Dropdown

**Reviewer:** Security discipline — inline review (read-only, with remediation by the owning teams).
**Date:** 2026-10-27
**Scope:** uncommitted tree implementing the web bot-scoping plan — `src/web.rs` (bot_select, bots_page/bots_save, bot_scope_control, bot_scope_sellers/in_bot_scope, effective_bot send adoption, page scoping), `src/auth.rs` (session extraction incl. `selected_bot_id`), `src/db.rs` bot helpers, `src/bots.rs`, `assets/app.css`, UX spec.
**Verdict:** **No blockers. Ship-allowed for pilot.** Three accepted notes, two follow-ups (one already tracked in the UX spec §7).

## Plan-mandated checks

### 1. Bot selector spoofing — pinned users cannot switch — PASS
- `bot_select` gates on `session.role != "owner"` → 403 `forbidden_page`; sellers/admins never receive a control (`bot_scope_control` returns the pinned label).
- Gated by test `web::tests::bot_select_rejects_pinned_roles` (POST attempt from a seller session leaves `selected_bot_id` NULL).
- Selection target validated server-side: bot must exist **and** be enabled; empty value resets to primary. Tests: `bot_select_refuses_disabled_and_unknown_bots`.

### 2. Redirect handling — PASS (no open redirect)
- `referer_path` discards client scheme/host entirely and returns only a path; requires the configured base-path prefix when mounted under one; caps length at 512. The browser can only ever be sent to this origin.
- Fallback is `{base}/dashboard`.

### 3. Cross-bot data leakage — PASS with one accepted note
- Owner + selected bot: plans/products/orders filter by `sellers_for_bot(selected)` at the handler level; channels filter by `for_channel` resolution. Unscoped pages (dashboard, codes, reviews, analytics, finance) remain global for a scoped owner — **note N1** (UX scoping gap, not a leak: the viewer is the same global owner; tracked in `BOT_SWITCHING_UX_SPEC.md` §7).
- Sellers/admins are untouched by the selection path: their queries stay seller-scoped as before; `bot_scope_sellers` returns `None` for non-owners, and `effective_bot` resolves sellers via `for_seller`, admins via `admins.bot_id`.
- **N2 (accepted, matches v1 contract):** admin page-data is global (the "admins share the privileged tier" contract). Admin pinning affects Telegram sends, not page scoping. Revisit together with N1 when an admin-bot setter exists in the data layer.

### 4. Injection / XSS — PASS
- Every dynamic interpolation in the new surface (`bot key`, `username` in selector options, pinned label, bots table) goes through `esc()`, consistent with the console's render idiom.
- SQL: all new writes flow through parameterized `Db` methods (`set_web_session_selected_bot`, `update_bot_settings` — existing, unit-tested).

### 5. CSRF — PASS under the pre-existing posture
- Session cookie is `HttpOnly; SameSite=Strict` (web.rs:991-997): cross-site POSTs to `/bot/select` or `/bots` do not carry the session. No new CSRF surface introduced; consistent with every other console POST handler.
- Worst-case in-posture impact of a scope change is self-scoped nuisance (an owner's own view), never privilege change.

### 6. Secrets — PASS
- The Bots page renders metadata only (id, key, username, owner, default channel, enabled); tokens are not stored in the `bots` table and the page's help copy points to `BOTS_CONFIG`/`BOTS_JSON` + restart. Token editing remains out of scope by design.
- New logging is a `tracing::info!` with `bot_id` only — no secrets, no tokens, no session material. `session_token_from_headers` hashes the token for the DB write and never logs it.

### 7. MCP bot-arg validation — deferred, consistent with plan (N3)
- Per the plan's non-goals, per-bot MCP endpoints and tool `bot` args are a follow-up. Verified the current posture: MCP tools scope by `seller_id` (src/mcp/tools.rs), and seller→bot resolution flows through the seller row. No new exposure introduced by this change.

## Notes register

| # | Severity | Finding | Disposition |
|---|---|---|---|
| N1 | Low (UX) | Dashboard/Codes/Reviews/Analytics/Finance not bot-filtered for a scoped owner | Accepted for v1; tracked in UX spec §7 — seller-set extension is mechanical but each page's aggregate math needs re-checking |
| N2 | Info | Admin page-data global; admin bot pinning affects sends only | Matches v1 `is_admin_role` privileged-tier contract; revisit with the future admin-bot setter |
| N3 | Info | MCP tool `bot` arg validation not implemented | Explicit plan non-goal; no regression (seller-scoped today) |

## Verification performed
- Read all touched handlers and helpers end-to-end (no rubber-stamping of the pre-existing plumbing: `effective_bot`'s pinned resolution was read line-by-line — seller→`for_seller`, admin→`admins.bot_id`→fallback primary, owner→selection→fallback primary).
- Gated by 8 new tests in `src/web.rs` (5 × server surface, 3 × web shell/resolution/scoping) plus the restored `Session` struct's downstream suite: **357 lib tests, 0 failures** at review time.
