# Architecture — Daybook

Multi-tenant platform for sellers to run subscription businesses on Telegram:
one Rust binary, three interfaces (bot + web + MCP), one SQLite database.

**New here?** Read this page top-to-bottom (~5 min), then `docs/EASY_SETUP.md`
to run your own instance. **Working on the code?** Read `AGENT.md` (standards)
before your first change.

## Quick picture

```
┌───────────────┐  ┌─────────────────┐  ┌────────────────────┐
│  Telegram Bot │  │  Web Dashboard  │  │  MCP (AI agents)   │
│  (teloxide)   │  │  (axum)         │  │  mcp/ (JSON-RPC)   │
│  handlers/    │  │  web/ + api.rs  │  │  /api/v1/mcp       │
└───────┬───────┘  └────────┬────────┘  └─────────┬──────────┘
        │                   │                     │
        └───────────────────┴──────────┬──────────┘
                              │  role check: roles::role_of()
                              │  + tiered tool permissions (mcp/)
                     ┌────────▼────────┐
                     │  Domain Core    │  db.rs · crypto.rs · ai/ · dialogue.rs
                     └────────┬────────┘
                              │  parameterized SQL, seller_id scoping
                     ┌────────▼────────┐
                     │  SQLite (WAL)   │  data/agent.db
                     └─────────────────┘
```

Three thin interfaces, one core. Interfaces parse input and render output; every
business rule and every write lives in the core, behind `Db` methods. That is
the rule that keeps the bot, the dashboard and the MCP endpoint from drifting
apart. The MCP interface is AI-facing: its tools are tiered (Read / Write /
Sensitive — see PLAN.md) and Sensitive tools never execute without an owner's
`/approve` in the bot.

## Modules

| Path | Owns |
|------|------|
| `src/main.rs` | bootstrap: db, bot task, web task, background jobs |
| `src/config.rs` | env config |
| `src/db.rs` | SQLite data layer — all persistence |
| `src/crypto.rs` | Crypto Pay HTTP client |
| `src/util.rs` | helpers |
| `src/roles.rs` | role resolution (Owner > Admin > Seller > User) |
| `src/dialogue.rs` | conversation state |
| `src/api.rs` | API surface |
| `src/mcp/` | MCP surface for AI agents — JSON-RPC, tool tiers, approval gate |
| `src/ai/` | AI providers (OpenAI/Anthropic), cost, rate limits |
| `src/handlers/` | bot commands, messages, callbacks |
| `src/web/` | axum routes, auth, HTML |

## Request flow

**Telegram:** update → `handlers/` dispatch → `roles::role_of()` gate →
`dialogue.rs` state machine (wizards) → `Db` methods → reply / inline keyboard.

**Web:** request → `src/web/` route → session cookie auth → `Db` methods →
server-rendered HTML (no SPA, no client-side state).

**Payment:** `/buy` → invoice row → Crypto Pay → background job polls/verifies
via `crypto.rs` **server-side** → subscription granted only after API
confirmation. Never trust a user's claim of payment.

## Data model

Multi-tenant: every business entity carries `seller_id`; superadmin
(`OWNER_ID`) sees everything. Tables: `sellers`, `config`, `posts`, `invoices`,
`admins`, `plans`, `subscriptions`, `activation_codes`, `code_redemptions`,
`users`. Migrations run on startup (`Db::migrate` from `schema.sql`).

## Deployment profiles — the same binary anywhere

| Where | Entry point |
|-------|-------------|
| Desktop (Win/mac/Linux) | `make package` → run the bundle (see `docs/DESKTOP.md`) |
| New server | `./setup.sh` + `make run-release` |
| **Existing server** | `sudo make install` — installs only missing parts, reuses nginx/caddy/`.env`/data (`scripts/install.sh`) |
| Docker | `docker compose up -d --build` |
| ARM | arm64 release artifact / build on device |

Full matrix and operational details: [`docs/DEPLOY_PROFILES.md`](./docs/DEPLOY_PROFILES.md).
Everything is one `make` away — `make help` lists all targets.

## Security

- Role gating on every privileged action (`roles::role_of()`).
- Payments verified server-side via Crypto Pay API before granting access.
- Web session cookies hashed (SHA-256); secrets only via env, never logged.
- Parameterized SQL everywhere; all writes through `Db` methods.
- Patched teloxide (unmaintained aquamarine removed); `cargo audit`/`deny` in gates.

## Setup in three commands

```bash
./setup.sh                 # interactive: token, owner id, password → .env (0600)
cargo build --release      # or: docker compose up -d --build
./target/release/daybook
```

Details: `docs/EASY_SETUP.md` · Operations: `docs/DEPLOYMENT.md` ·
Security: `docs/SECURITY.md`.

## Distribution: server or desktop app

One codebase, two deployment shapes:

- **Server** — Docker Compose / systemd; dashboard on `WEB_BIND=0.0.0.0`
  behind a TLS proxy. See `docs/DEPLOYMENT.md`.
- **Desktop app** — the same binary packaged per platform with
  `scripts/package.sh`: `daybook.exe` (embedded icon/version resources)
  on Windows, `Daybook.app` bundle on macOS, tar.gz on Linux. The
  dashboard binds `127.0.0.1` and opens in the system browser automatically.
  Data resolves portable-first (`.env`/`data/` beside the binary), else the
  platform dir (`%APPDATA%`, `~/Library/Application Support`, `~/.local/share`).
  Release CI (`.github/workflows/release.yml`) builds all three on a `v*` tag.
  See `docs/DESKTOP.md`.

## Production checklist

- [ ] `.env` mode 600; bot token and dashboard password set.
- [ ] `data/` backed up (it is the whole business state).
- [ ] `restart: unless-stopped` (compose) or systemd unit.
- [ ] Healthcheck green (`GET /`).
- [ ] `RUST_LOG=info`; alerts on payment-job errors.
- [ ] Quality gates green before every deploy.

## Scalability

- **v1:** single SQLite (WAL + busy_timeout), single process — good for
  thousands of sellers. Command response < 2s; DB p99 < 50ms; < 64MB idle.
- **v2:** Postgres, horizontal bot workers, Redis cache, read replicas.

## Non-goals (v1)

No Kubernetes, no microservices, no ML, no chat beyond Telegram.
