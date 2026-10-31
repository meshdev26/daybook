# Product Plan — Telegram Business Platform

## Vision
A **general-purpose, multi-tenant Telegram business platform** that any seller,
creator, or business can use to sell access, products, and services — managed
from Telegram and a web dashboard, deployable anywhere.

## Where we are (2026-10-27) — v1.6 multi-bot foundation shipped

**Business types covered:** channel subscriptions/memberships ✅ · activation
codes ✅ · one-time digital products (link/file/text, auto-delivered) ✅ ·
physical products with order tracking ✅ · refunds/payouts with owner-approved
Crypto Pay transfers ✅ · **bookings = the remaining type (v2)**.

**Platform:** single Rust binary (lib + bin) — desktop (.exe/.app/tar.gz,
release CI for win/mac/linux x64+arm64), existing-server installer with
hardened systemd unit, Docker, subpath mounting (`WEB_BASE_PATH`), embeddable
(`embed::AgentAppBuilder`), compile-time command registry for extensions.

**Engineering:** role-level architecture (L3–L11) machine-checked by
`scripts/verify-agent-tiers.py`; security floor remediated same-day (CSPRNG,
atomic + fail-closed payment claims, setup gating, XSS, throttling); 348 tests
incl. property invariants + libFuzzer; WAL-safe `make backup`; `make doctor`
diagnostics; full gate suite in `make test`.

Docs: `ARCHITECTURE.md` (system map), `AGENT.md` (handbook + decision log),
`AUTO_ROUTE.md` (routing), `docs/ARCHITECTURE-SCALE-PLAN.md` (fullness audit,
gap designs, scale seam contracts), `docs/DEPLOY_PROFILES.md`, `docs/DESKTOP.md`.

**2026-10-27 addenda:** `/refundapprove` and `/payoutapprove` bot commands
(owner/admin, approve-only — execution stays on the console Finance page /
REST execute endpoint); the seller AI-settings endpoints
(`GET/POST /sellers/{user_id}/ai-settings`) and the code-redemptions endpoint
(`GET /codes/{code}/redemptions`) are implemented; new web API-keys page
(`/keys`); `[@channel]` commands now enforce seller channel ownership (a
seller can target only their own channel, never the global default).
Same-day security-remediation wave: CAS-guarded money transfers (`executing`
claim state, ambiguous-failure semantics, attempt/ambiguous audit events),
REST MFA parity (`POST /auth/mfa/verify` pending-token exchange), channel
binding proof-of-control, security-headers middleware, and a WCAG AA floor in
the console (contrast tokens, labelled controls, skip link).

## Refactor roadmap — module split

Enforces `AUTO_ROUTE.md` §8 (file & module hygiene). Owner: **L11** plans,
**L7/L8/L9** execute, **L3** verifies every wave. Two phases, deliberately
separated: Phase A is mechanical and safe today; Phase B is a design change and
needs its own permission-map expansion first.

### Current size debt (tracked, not assumed)

Refreshed 2026-10-30 (verified `wc -l` on the working tree, after the web
module split and the setup/management UX polish wave; test modules live in
sibling `tests.rs`/`tests/` files per Phase A).

| File | Owner | Prod lines | Status |
|------|-------|------------|--------|
| `src/db.rs` | L9 | 7,512 | over hard cap — Phase B domain split pending |
| `src/handlers/commands.rs` | L8 | 5,013 | over hard cap — Phase B domain split pending |
| `src/api.rs` | L8 | 4,638 | over hard cap — Phase B domain split pending |
| `src/web/bots_admin.rs` | L7 | 1,094 | over 800 target, under cap — boy-scout split on next touch |
| `src/web.rs` | L7 | — | RESOLVED: split into `src/web/`; every other surface ≤ 800 target |

None of these grow further; each is reduced when next touched (boy-scout rule).

### Phase A — extract the inline test modules (execute now)

Move-only. Each parent file shrinks to `prod + 1` lines, tests keep compiling
unchanged via `use super::*;`.

| Parent (façade edit) | New file | Removed from parent |
|----------------------|----------|---------------------|
| `src/web.rs` → replace inline block with `#[cfg(test)] mod tests;` | `src/web/tests.rs` | lines 11,827–16,490 |
| `src/db.rs` → `#[cfg(test)] mod tests;` | `src/db/tests.rs` | lines 7,408–10,358 |
| `src/api.rs` → `#[cfg(test)] mod tests;` | `src/api/tests.rs` | lines 4,101–6,798 |
| `src/handlers/commands.rs` → `#[cfg(test)] mod tests;` | `src/handlers/commands/tests.rs` | lines 5,012–5,409 |

Permission-map globs required **before** the move lands (orchestrator expands
`.opencode/agents/*.md` and `.qoder/agents/*.md`; L11 cannot):

- L7 (`staff-client-platforms`): add `src/web/**`.
- L9 (`staff-core-systems`): add `src/db/**`.
- L8 (`staff-server`): add `src/api/**` and `src/handlers/commands/**`.

Note: `src/web/tests.rs` and `src/db/tests.rs` need those same globs even though
the parent file stays in the existing allowlist — without them the owning rung
cannot create the new file.

### Phase B — production split (deferred; per-module map expansion)

Each parent becomes a façade (`pub use` re-exports, unchanged public paths);
each target file carries `impl` blocks for the same type. Group by domain, not
by line count.

- `src/web.rs` → façade +
  `src/web/{auth,session,render,shell,scoping,finance,bots}.rs` +
  `src/web/pages/*.rs` (per console page) + `src/web/routes/*.rs` (handlers per
  area). Session/auth, HTML render helpers, bot-scope resolution and the money
  pages are the four seams that carry security weight — they split first.
- `src/db.rs` → façade + `src/db/{accounts,sellers,channels,plans_products,
  invoices,subscriptions,codes,bookings,posts,mcp_tokens,audit}.rs`, one
  `impl Db` block per file. All SQL stays in this layer (single source of truth).
- `src/api.rs` → façade + `src/api/{auth,channels,posts,finance,subscriptions,
  codes,mcp_adjacent}.rs`.
- `src/handlers/commands.rs` → façade +
  `src/handlers/commands/{admin,seller,user,commerce}.rs`; the compile-time
  registry entries (`handlers::registry`) are preserved exactly — command names,
  tiers and router wiring do not change.

### Acceptance criteria (both phases)

1. Move-only diff: no logic edits, no renamed public items, no changed lines
   inside moved code other than paths/imports.
2. The 469 lib tests still pass; test count unchanged.
3. `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt
   --check`, `cargo audit`, `cargo deny check` green.
4. `python3 scripts/verify-agent-tiers.py` green.
5. Public API unchanged from outside the crate (façade `pub use` keeps every
   existing path resolving).

### Sequencing

Phase A → **L3 verify** → Phase B wave 1 (`src/web.rs` + `src/db.rs`, disjoint)
→ **L3** → Phase B wave 2 (`src/api.rs` + `src/handlers/commands.rs`, disjoint)
→ **L3**. Waves are disjoint file sets, so they can run in parallel within a
wave; never two waves at once.

### Risk — read before starting Phase B

The working tree currently carries a large uncommitted feature change. Stacking
a move-only split on top of it makes both unreviewable: a reviewer cannot tell
a pure move from a semantic edit, and the Phase B acceptance criteria become
unverifiable in practice. **Recommendation: the owner commits (or stashes) the
verified feature first** — a human action, not an agent's. Phase A is safe to
prepare alongside but should also land on a clean tree before Phase B begins.

## Next milestones

### Product roadmap (from product review, 2026-10-25 — adoption-ranked)
1. **Front door (blocking):** push to GitHub + tag; README screenshots, 60-second
   intro, demo. A product without a front door is a private project.
2. **Owner approvals in Telegram ✅ shipped (approve-only):** `/refundapprove`,
   `/payoutapprove` bot commands (owner/admin-gated) — money decisions belong
   where the owner lives, on their phone. Approval only: the transfer itself
   is executed from the web console Finance page or the REST execute endpoint.
3. **Empty-state onboarding:** first-login checklist (create first product →
   test-buy → verify delivery); measure time-to-first-sale (<10 min target).
4. **Trust pack:** public security summary, changelog automation, versioned
   releases; external pentest before significant real money.
5. **Seller analytics:** revenue/subscriber/churn views in the dashboard
   (data exists via API; surface it).
6. **i18n:** Telegram commerce is heavily non-English (RU/FA/ES/ID first).

### v1.3.x (polish, on demand)
- `/buy seller/name` disambiguation syntax (S).
- Metrics/tracing spans for the performance budgets (observability) (S/M).
- Remove legacy `/process` refund endpoints (S). Changelog automation (S).
- First real-world pilot: one bot, one channel, one digital + one physical
  product, one refund, backup-restore drill.
- **MCP server (v1.3.x) ✅ shipped.** The fourth thin interface over the
  domain core: a hand-rolled MCP (JSON-RPC) endpoint at `POST /api/v1/mcp`
  inside the existing axum router — zero new dependencies, same `WebState`
  (db + cfg + bot). Designed AI-first, because the primary caller is an
  AI agent, not a human:
  - **Auth:** `MCP_OWNER_TOKEN` env (static bearer → Owner principal).
    Empty/unset = endpoint disabled entirely (secure default OFF).
    Constant-time comparison. v2: a `mcp_tokens` table with per-seller
    tokens issued via bot command; `Principal { role, seller_id }` is
    threaded through every tool now so that is additive, not a redesign.
  - **Tool tiers — the permission model:**
    - *Read* (`readOnlyHint`, direct): list_channels, list_plans,
      list_posts, subs_report (user ids masked by default), list_codes
      (codes masked by default).
    - *Write* (direct, audited): publish_post, edit_post.
    - *Sensitive* (`destructiveHint`, **server-side approval gate**):
      gen_codes (returns secrets), delete_post (destructive), broadcast
      (high blast radius). Calling one creates a pending row and returns
      `requires_approval` — nothing executes. The OWNER approves via the
      bot (`/approvals`, `/approve <id>`, `/deny <id>`, owner-only,
      15-minute expiry); execution happens on approval, and secrets
      (generated codes) are delivered to the owner's Telegram chat, never
      back into the AI's transcript.
  - **Audit:** every tool call and every approve/deny lands in
    `mcp_audit` (tool, role, ok, truncated detail).
  - **Rate limit:** reuse the AI rate-limiter pattern on the endpoint.
  - Verification: in-process axum oneshot tests (no network, no real bot) —
    auth, protocol methods, tier annotations, masking, the full
    approval lifecycle, audit rows.
- **Setup becomes skippable; configuration moves in-console (v1.3.x) ✅ shipped.**
  The first-run wall is wrong for a product whose bot can also be
  configured later. New flow, security floor intact (B2 semantics):
  1. `/setup` gains **Skip for now** — it sets `setup_done=1` and creates
     NO owner account. The bot simply stays unconnected (the existing
     "setup mode" polling behavior).
  2. With no owner account, `/login` becomes **claim mode**: create the
     owner account. Anti-takeover (the B2 lesson applies to claims too):
     claiming requires a one-time **claim code** generated at startup and
     printed to the server console — unless the request arrives from
     loopback (desktop default binds 127.0.0.1, where claiming with no
     code is the whole point). Once claimed, claim mode never returns.
  3. **Settings** (`/settings`, owner-only, in-console): bot token
     (write-only — never rendered back), owner ID, default channel,
     Crypto Pay token, dashboard password. Saves reuse the setup .env
     writer (extracted, key-preserving, 0600) and say honestly: the bot
     connects on restart.
  4. **Channels become managed data**: a `channels` table (channel,
     seller_id 0 = global, is_default). Owner manages global channels
     (add / set default / remove) in the console; sellers keep their
     bot-managed channel, shown read-only. The default-channel readers
     (`get_config("channel")` call sites) resolve through the table with
     legacy-config fallback. The dashboard Post form gets a channel
     dropdown instead of a free-text guess.
  - Orchestration: L9 core/data (semantics, claim code, channels store,
    env-writer extraction) + L7 web (skip, claim UI, settings, channels
    manager); bot command surface unchanged in v1.

### v1.3.x shipped addenda

- **Orders-lite (v1.3) ✅ shipped.** Physical product purchases create an `orders`
  row; `/orders` lists (seller-scoped) and `/orderset <order_id> <status> [tracking]`
  updates status (`pending|shipped|delivered|refunded|cancelled`) with optional
  tracking; web dashboard **Orders** page.
- **send_document delivery (v1.3) ✅ shipped.** `digital_file` products now re-send
  the actual Telegram document via `send_document` when the payload is a `file_id`
  (link/text payloads fall back to a message); see `services::deliver`.

### v1.6 (multi-bot, config-driven) ✅ foundation shipped
- **Multi-bot support (v1.6).** One process runs N Telegram bot tokens, each
  with its own owner, default channel, and (via the existing seller model) its
  own channels/plans/products/services — sharing one SQLite DB and one web
  console. Implemented as the smallest viable step:
  - **Schema (L9):** `bots` table + nullable `bot_id` on `sellers`,
    `channels`, `admins`, `web_sessions.selected_bot_id`, `mcp_audit.bot_id`.
    No rebuilds; legacy single-bot installs backfill to a synthesized
    "primary" bot (byte-identical behavior).
  - **Config:** `SharedConfig`/`BotConfig`/`AppConfig` split; `BOTS_CONFIG`
    (TOML) / `BOTS_JSON` (env) / legacy single-bot env (`TELEGRAM_BOT_TOKEN`
    + `OWNER_ID`) synthesized as primary. Tokens stay in env/config, never DB.
  - **BotContext / BotManager:** per-bot context (id, key, Bot handle, owner,
    shared cfg, db); resolvers `for_seller`, `for_channel`, `for_bot_key`,
    `get`. One Dispatcher per bot spawned as separate tokio tasks.
  - **Roles:** `is_admin(user, bot_id)` bot-aware (NULL = global admin);
    `role_of` takes the bot's owner.
  - **Web:** single dashboard with bot selector for the global owner; bot
    owners/admins pinned to their bot. `state.bot` stays primary during
    transition.
  - **MCP:** single endpoint; owner token global; seller tokens resolve bot
    via seller; optional `bot` arg on Telegram-acting tools (spoof-resistant).
  - **Jobs:** per-bot invoice-grant sweeper; other sweepers on primary with
    TODO for full per-bot migration.
  - **Remaining (L8/L7/L3):** handler `bot_ctx` injection, web bot selector +
    scoped pages, MCP bot arg handling, per-bot sweepers for all jobs, and
    multi-bot integration tests.
- **Design doc:** `docs/` (mirrored at `~/.commandcode/plans/multi-bot-design.md`).

### v2 (trigger-driven)
- Bookings: services/slots/bookings reusing the invoice pipeline.
- Scale seams — **only on trigger** (>1k active sellers or DB p99 > 50ms):
  `Db` trait extraction (Postgres-ready), services layer expansion, worker
  split, connection pooling. Contracts pre-documented in the scale plan.

### Owner decisions (business, not code)
1. Go-live with real transfers (T2 — small test amount first).
2. Code-signing certificates (SmartScreen/Gatekeeper).
3. Hosted/SaaS offering for non-technical sellers.

## Success metrics (unchanged)
- Time to first sale < 10 minutes · zero-config onboarding · 99.9% uptime
  (hosted version).
