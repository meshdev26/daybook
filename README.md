# Daybook — Telegram commerce back-office

Daybook — the calm back-office ledger for your Telegram commerce.

An **owner-only** Telegram bot that:
- Posts and manages a target Telegram channel **only on your command**.
- Sells subscriptions and grants channel access via **Crypto Pay** or **activation codes**.
- Stays **silent to everyone else**.

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

### Channel management (multi-channel)
- `/channels` — list known channels.
- `/setchannel @chan|id` — set default target channel.
- Commands accept an optional leading `@channel` to target a specific channel, e.g. `/post @mychannel Hello`.
- `/channel`, `/info` — show channel info.
- `/post <text>`, `/postphoto`, `/edit`, `/delete`, `/pin`, `/unpin`, `/unpinmsg`, `/invite`.
- `/settitle`, `/setdesc`.

### Subscriptions & auto-selling
- `/plans` — list plans.
- `/planadd name | price | asset | days` — create plan.
- `/plandel name` — remove plan.
- `/buy <plan>` — create Crypto Pay invoice; user pays; run `/paid <invoice_id>` to verify and grant access.
- `/grant user_id | days` — manual access grant.
- `/revoke user_id` — revoke access (kick).
- `/subs` — list subscriptions.
- `/expirecheck` — revoke expired subscriptions.

### Activation codes
- `/gencodes plan | count | [days]` — generate codes.
- `/codes` — list codes.
- `/redeem CODE` — redeem code for access.

### Crypto Pay
- `/crypto setup <token>`, `/crypto me|balance|rates|invoices|invoice|check`.

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
./target/release/telegram-agent
```
Web dashboard: http://localhost:8080 (set `WEB_PORT`, `WEB_USERNAME`, `WEB_PASSWORD` in `.env`). Login with owner credentials; sellers get accounts via `/becomeseller` or `/dashboard`.

## MCP server (AI agents)

Expose the operator as tools to AI agents (ZCode, Claude, Cursor, …) via the
[Model Context Protocol](https://modelcontextprotocol.io): `POST /api/v1/mcp`.
Set `MCP_OWNER_TOKEN` in `.env` (empty = endpoint disabled, secure default OFF),
then register the server in your agent's MCP settings:

```json
{ "telegram-agent": { "url": "http://127.0.0.1:8080/api/v1/mcp",
                      "headers": { "Authorization": "Bearer <MCP_OWNER_TOKEN>" } } }
```

The token maps to the **Owner** principal. Tools are tiered so AI use stays safe:

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
4. User (or you) runs `/paid <invoice_id>` → bot verifies payment, creates subscription, sends single-use invite link to the private channel.
5. On expiry, run `/expirecheck` (or schedule) to revoke access.

Activation codes provide an alternative: `/gencodes Premium | 5` then share codes; users run `/redeem CODE`.

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
- Validate payments via Crypto Pay API before granting access.
- Run `/expirecheck` periodically (cron) to revoke expired users.

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
- `CONTRIBUTING.md` — contributor guide: setup, rules, and how to propose a change.
- `PLAN.md` — product vision & roadmap.
- `ARCHITECTURE.md` — system design.
- `docs/COMMANDS.md` — command reference.
- `docs/SETUP.md` — setup guide.
- `docs/MULTI_BOT.md` — running several bot tokens in one instance.
- `docs/MULTIBOT_FOUNDATION.md` — product types, capability builds, and moderation.
- `docs/ARCHITECTURE.md` — code structure.
- `docs/CRYPTO_PAY.md` — Crypto Pay integration.
- `docs/SECURITY.md` — security model.
- `docs/DEPLOYMENT.md` — deployment guide.
- `docs/DEPLOY_PROFILES.md` — same app anywhere: desktop / new / existing server, Docker, ARM.
- `docs/DESKTOP.md` — desktop app distribution (.exe / .app).
- `docs/reviews/2026-10-25-architecture.md` — architecture review & v1.1 plan.
- `docs/ARCHITECTURE-SCALE-PLAN.md` — fullness verdict + products/orders/bookings roadmap (awaiting approval).
- `docs/COMMANDS.md` — full command reference.
- `docs/SETUP.md` — step-by-step setup.
- `docs/ARCHITECTURE.md` — code structure.
- `docs/CRYPTO_PAY.md` — Crypto Pay integration details.