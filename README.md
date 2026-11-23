# Daybook — Telegram commerce back-office

Daybook — the calm back-office ledger for your Telegram commerce.

> **Alpha:** self-hosted, self-reviewed (with AI assistance), not independently audited. Do not use with real customer money or live customer data without first reading the code and testing seller isolation on your own deployment.
>
> Track the pre-real-money checklist in [issue #1](https://github.com/meshdev26/daybook/issues/1).

Updates channel: https://t.me/daybook26

A self-hosted Telegram bot and web console for selling paid-channel access. The owner runs it; sellers manage their own channels; customers only see the shop commands.
- Posts and manages a target Telegram channel **only on command**.
- Sells subscriptions and grants channel access via **Crypto Pay** or **activation codes**.

## Features

### Multi-tenant (many sellers)
- Any user can `/becomeseller` then `/setup @channel` to start selling.
- Each seller has isolated channels, plans, codes, customers, and broadcasts.
- Superadmin (OWNER_ID) can see everything.

### Easy setup for non-technical users
- `/setup @yourchannel` links a channel in one step.
- `/planadd name | price | asset | days` creates a plan.
- `/gencodes plan | count` generates activation codes.

### Two modes
- **Public (user side)**: any user can `/start`, browse `/plans` and `/shop`, `/buy`, `/paid`, `/redeem`, plus `/services` and `/book`.
- **Admin (server side)**: sellers/admins manage their own channels, plans, codes, broadcasts.
- `/broadcast <text>` sends a message to a seller's customers (admin: all users).
- Refunds and payouts are **approve-only from the bot**: `/refundapprove <id>` and `/payoutapprove <id>` approve a pending request; the bot never moves money. Transfers run from the web console Finance page or REST, where an ambiguous transfer outcome parks the payout/refund in an `executing` state for operator reconciliation.

### Services & bookings
- `/services` — list bookable services; `/book <service>` — request a slot.
- `/mybookings` — your bookings; `/cancelbooking <id>` — cancel one.
- `/bookings` (alias `/books`) — seller: own bookings; admin/owner: all.

### Channel management (multi-channel)
- `/channels` — list known channels.
- `/setchannel @chan|id` — set default target channel.
- Commands accept an optional leading `@channel` to target a specific channel, e.g. `/post @mychannel Hello`.
- `/channel`, `/info` — show channel info.
- `/post <text>`, `/postphoto`, `/edit`, `/delete`, `/pin`, `/unpin`, `/unpinmsg`, `/invite`.
- `/settitle`, `/setdesc`.
- `/schedule [@channel] <YYYY-MM-DD HH:MM> | <text>` schedules a post (UTC); `/scheduled` lists pending ones, `/cancelpost <id>` cancels one.

### Subscriptions & auto-selling
- `/plans` — list plans.
- `/planadd name | price | asset | days` — create plan.
- `/plandel name` — remove plan.
- `/buy <plan>` — create Crypto Pay invoice. Payment verification via `/paid <invoice_id>` is a manual step today.
- `/paid <invoice_id>` — manually verify payment and grant access; a signed Crypto Pay webhook can enqueue paid invoices for the background granter, which re-verifies with Crypto Pay before granting (see Roadmap).
- `/grant user_id | days` — manual access grant.
- `/revoke user_id` — revoke access (kick).
- `/subs` — list subscriptions.
- `/expirecheck` — manual trigger only; expired access is revoked automatically by the in-process 6h sweeper.

### Activation codes
- `/gencodes plan | count | [days]` — generate codes.
- `/codes` — list codes.
- `/redeem CODE` — redeem code for access.

### Crypto Pay
- `/crypto me|balance|rates|invoices|invoice|check`. Set the token in the web console's `/crypto` page or via `CRYPTO_PAY_TOKEN`.

### Bot setup
- `/setupbot` — configure commands, description, short description.

### General
- `/start`, `/menu`, `/help`.

## Requirements
- Rust (stable)
- Telegram bot token from @BotFather
- Your Telegram user ID (owner)
- Optional: Crypto Pay token from @CryptoBot

## Configuration
Copy `.env.example` to `.env` and fill:

```
TELEGRAM_BOT_TOKEN=...
OWNER_ID=123456789
DATABASE_URL=sqlite:./data/agent.db
DEFAULT_CHANNEL=@yourchannel
CRYPTO_PAY_TOKEN=...
RUST_LOG=info
```

## Multiple bots

One process, one database, one console — several bot tokens. Describe them in a
TOML file (`BOTS_CONFIG`) or a `BOTS_JSON` array; tokens stay in the environment
via `token_env`, never in the database. The legacy
`TELEGRAM_BOT_TOKEN`/`OWNER_ID` pair keeps working unchanged as one bot keyed
`primary`. Business data stays seller-scoped and carries the bot through
`sellers.bot_id`.

Inside the console, the sidebar Bot selector lets the global owner read the
pages scoped to a single bot; sellers and admins see a pinned label instead
(their bot is resolved server-side and cannot be switched).

Adding or retokenizing a bot needs a restart. Full guide:
[docs/MULTI_BOT.md](docs/MULTI_BOT.md).

### Product types & capability builds

Bots carry a product type (`shop_bot` by default); the console Product / Type
selector lets the owner pick a product while sellers and admins stay pinned.
Capabilities are chosen at build time with Cargo features: `shop_bot`,
`moderation` (both default), `movie_bot`, `reply_bot`, `file_share`, `web_app`,
`service`, and `automation`. These features control which capabilities are
registered and which bot types are accepted at runtime — a movie-only build
registers only the movie capability and refuses other types. They do **not**
omit dependencies or shrink the binary (all crates stay compiled); dropping
unused dependencies is a deferred follow-up. A movie-only install
builds with `cargo build --no-default-features --features movie_bot`; changing
capabilities means rebuilding and redeploying.

Only `shop_bot` has a live vertical — a real inbound command and dialogue tree.
The other six types (`movie_bot`, `reply_bot`, `file_share`, `web_app`, `service`,
`automation`) are build-time registration types only: inbound updates to a bot
of one of these types return an explicit "not available" notice (`VerticalStub`)
instead of running a product. They are not shipped products.

Message moderation uses an owner floor, seller "Blocked words", and per-bot
overrides — set at least one floor term before enabling it. Full guide:
[docs/MULTIBOT_FOUNDATION.md](docs/MULTIBOT_FOUNDATION.md).

## Build & run
```bash
cargo build --release
./target/release/daybook
```
Web dashboard: loopback by default (set `WEB_PORT`, `WEB_USERNAME`, `WEB_PASSWORD` in `.env`). Login with owner credentials; sellers get accounts via `/becomeseller` or `/dashboard`. Set `WEB_URL` to a seller-reachable URL so those commands can issue one-time claim links; with it unset they refuse setup and never DM a password. A loopback URL is not a usable default — `127.0.0.1` in a Telegram DM points at the recipient's machine. For public access, put a reverse proxy with TLS in front — see [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) (`WEB_BIND`, `WEB_URL`, `WEB_BASE_PATH`).
Web MFA issues 10 one-time recovery codes drawn from the BIP-39 English wordlist, bcrypt-hashed at rest and shown once. `MFA_RECOVERY_CODES_ENABLED` defaults to on; setting it to `0`, `false`, `no` or `off` rejects stored codes and removes the recovery UI and route. TOTP sign-in is unaffected, so disabling the feature without a working authenticator can lock an account out.

## MCP server (AI agents)

Expose the operator as tools to AI agents (ZCode, Claude, Cursor, …) via the
[Model Context Protocol](https://modelcontextprotocol.io): `POST /api/v1/mcp`.
Set `MCP_OWNER_TOKEN` in `.env` (empty = endpoint disabled, secure default OFF),
then register the server in your agent's MCP settings:

```json
{ "daybook": { "url": "http://127.0.0.1:8080/api/v1/mcp",
                      "headers": { "Authorization": "Bearer <MCP_OWNER_TOKEN>" } } }
```

The token maps to the **Owner** principal. The registry exposes 38 tools across three tiers (24 Read, 4 Write, 10 Sensitive); the table highlights representative ones. Tiering keeps AI use safe:

| Tier | Tools | Behavior |
|------|-------|----------|
| Read | `list_channels`, `list_plans`, `list_posts`, `subs_report`, `list_codes` | Direct; PII and activation codes **masked by default** |
| Write | `publish_post`, `edit_post` | Direct; every call audited |
| Sensitive | `gen_codes`, `delete_post`, `broadcast`, `ai_publish_post` | **Never execute on the AI's say-so**: the call parks a pending approval and the AI is told to wait; **you** approve via `/approve <id>` in the bot (owner-only, 15-minute expiry). Generated codes are delivered to your Telegram chat — never into the AI's transcript |

Every tool call and every approve/deny is written to the `mcp_audit` table.

## How selling works
1. Create a plan: `/planadd Premium | 10 | USDT | 30`.
2. User buys: `/buy Premium` → bot returns pay link + invoice id.
3. User pays via Crypto Pay.
4. User (or you) runs `/paid <invoice_id>` to manually verify payment, create the subscription, and send a single-use invite link to the private channel. This is a manual step today.
5. Access is revoked automatically on expiry; `/expirecheck` is only a manual trigger.

Activation codes provide an alternative: `/gencodes Premium | 5` then share codes; users run `/redeem CODE`.

## Roadmap
- Validate Crypto Pay webhook confirmation end-to-end so the manual `/paid` verification step can be retired: the signed webhook at `POST /api/v1/webhooks/cryptopay` already enqueues paid invoices for the background granter, which re-verifies with Crypto Pay before granting (tracked in [issue #1](https://github.com/meshdev26/daybook/issues/1)).

## Architecture
- `src/main.rs` — dispatcher, dialogue storage; merges the routers with the request-timeout and metrics layers.
- `src/web/` — server-rendered web dashboard (axum pages, sessions, static assets), split per surface: `mod.rs` routing/auth/sessions, `dashboard.rs`, `channel_ops.rs` (first-run setup + channel ops), `settings.rs`, `bots_admin.rs` (multi-bot pages and add wizard), `bots_config.rs` (bots.toml writer/validator), `admin_console.rs` (grant/broadcast/admins/sellers), `wizard.rs`/`identity.rs`, `bookings.rs`, `finance.rs`, etc.
- `src/api.rs` — REST API under `/api/v1/...` (JSON), plus `/health` and `/metrics`.
- `src/mcp/` — MCP JSON-RPC 2.0 endpoint at `/api/v1/mcp` (tiered tools, audited).
- `src/db.rs` — SQLite persistence (plans, subscriptions, codes, invoices, config).
- `src/crypto.rs` — Crypto Pay API client.
- `src/handlers/commands.rs` — command handler + access grant.
- `src/handlers/messages.rs` — photo wizard.
- `src/handlers/callbacks.rs` — inline buttons.
- `schema.sql` — database schema.

## Security
- Owner/admin gating via `OWNER_ID` + admins table.
- Never commit `.env`.
- Sensitive config secrets (e.g. the Crypto Pay token saved in the console) are encrypted at rest with AES-256-GCM under `MASTER_KEY`; set it before saving secrets. Without it, secret writes are refused.
- Validate payments via Crypto Pay API before granting access.
- Expired access is revoked in-process by a 6h sweeper; the 30s loop verifies invoice payments. `/expirecheck` is a manual trigger.

## Known limitations

Alpha software; verify these on your own deployment before handling real value:

- No independent multi-tenant penetration test has been run; seller isolation is self-reviewed only (pre-real-money checklist in issue #1).
- The SQLite database file itself is not encrypted; filesystem permissions still matter. Sensitive config values are encrypted individually under `MASTER_KEY`, and legacy plaintext values keep working until re-saved. Changing or losing the key makes previously encrypted values unreadable.
- The web dashboard relies on `SameSite=Strict` cookies for CSRF protection and emits no per-form CSRF tokens.
- Dashboard account setup requires `WEB_URL`: with it unset, `/becomeseller` and `/dashboard` refuse setup and never DM a password. Set a seller-reachable URL (not loopback) to enable one-time claim links.
- The signed webhook path (signature, queueing, atomic claim, provider verification and exactly-once grant with replay protection) is covered by a deterministic local-mock integration test; the background granter's Telegram invite and DM side effects are not exercised by that test.
- Revenue is reported per asset; mixed-asset responses carry a marker instead of a summed float. Never interpret a single money number across denominations.

## Deployment
- Any Linux VPS/home server/Raspberry Pi.
- Docker: build release binary in multi-stage image.
- Run under systemd for auto-restart.

## One-command operations (Makefile)

```
make setup      # interactive first-run configuration
make run        # build & run locally          make test      # full quality gates
make package    # desktop bundle (.exe/.app/tar.gz)
make install    # install onto an existing Linux server (systemd, hardened)
make doctor     # environment diagnostics      make status    # install inventory
```

Deploy anywhere — desktop, new VPS, existing server, Docker, ARM:
see [docs/DEPLOY_PROFILES.md](docs/DEPLOY_PROFILES.md).

## Documentation
See `docs/` and root MD files:
- `QUICKSTART.md` — zero to a running bot in five minutes.
- `CONTRIBUTING.md` — contributor guide: setup, rules, and how to propose a change.
- `docs/COMMUNITY.md` — updates-channel decision, platform, and charter.
- `docs/LEGAL.md` — legal posture (free library; operator responsibilities).
- `PLAN.md` — product vision & roadmap.
- `ARCHITECTURE.md` — system design.
- `docs/COMMANDS.md` — command reference.
- `docs/SETUP.md` — setup guide.
- `docs/MULTI_BOT.md` — running several bot tokens in one instance.
- `docs/MULTIBOT_FOUNDATION.md` — product types, capability builds, and moderation.
- `docs/CRYPTO_PAY.md` — Crypto Pay integration.
- `docs/SECURITY.md` — security model.
- `docs/DEPLOYMENT.md` — deployment guide.
- `docs/DEPLOY_PROFILES.md` — same app anywhere: desktop / new / existing server, Docker, ARM.
- `docs/DESKTOP.md` — desktop app distribution (.exe / .app).
- `docs/reviews/2026-10-25-architecture.md` — architecture review & v1.1 plan.
- `docs/ARCHITECTURE-SCALE-PLAN.md` — fullness verdict + products/orders/bookings roadmap.