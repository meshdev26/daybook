# Easy Setup Experience — Phased Plan

## Goal
Non-technical business owners can deploy and configure the Telegram Business Platform
without editing `.env` or touching code. Single binary preserved. Secrets stay in
environment/`.env` (not DB) whenever possible; DB only holds non-secret config and
encrypted-at-rest placeholders if env is unavailable.

## Phase 0 — Defaults & validation (config.rs)
- `Config::from_env()` no longer hard-fails on missing `TELEGRAM_BOT_TOKEN`/`OWNER_ID`;
  returns a config with `is_configured() == false` so the app can boot into setup mode.
- Sensible defaults: `WEB_PORT=8080`, `WEB_USERNAME=owner`, DB path `sqlite:./data/agent.db`,
  `AI_MODEL=gpt-4o-mini`, `AI_BASE_URL=https://api.openai.com/v1`.
- Helpers: `Config::web_port()`, `Config::web_username()`, `Config::is_configured()`,
  `Config::web_url()`.
- Validation helpers (used by wizard): bot token validated against Telegram `getMe`;
  owner ID must be positive integer; channel must start with `@` or be numeric chat id;
  dashboard password >= 6 chars.

## Phase 1 — First-run web setup wizard (`src/web/channel_ops.rs`)
- Route `/setup` (GET/POST) available only when `!cfg.is_configured()`.
- `index` and `login_page` redirect to `/setup` in setup mode.
- Form is grouped for a first-run owner: **Your bot** (bot token, owner Telegram
  user ID) and **Your console login** (dashboard username/password); the optional
  fields (default channel, Crypto Pay token) are collapsed under
  "Optional — add later in Settings".
- On submit:
  - Local validation runs first — token present, owner ID a positive integer,
    dashboard password at least 6 characters — and every problem is reported in
    one alert before Telegram is contacted.
  - Validate bot token via `https://api.telegram.org/bot<token>/getMe`
    (rejected vs. unreachable get distinct, honest error copy).
  - Any failure re-renders the page in place with what you typed preserved
    (owner ID, default channel, dashboard username); the token and password
    fields are re-entered for safety — never a link back to a blank form.
  - Write `.env` atomically: the content is staged in a sibling temp file created with mode 0600, then renamed over the target (secrets remain in env file, NOT DB).
  - Seed owner web account + default channel in DB.
  - Success page states what works right now (the console login) vs. what needs
    the restart (the bot polling), with per-platform restart commands
    (Desktop: quit and reopen; systemd: `systemctl restart daybook`;
    Docker: `docker compose restart`).
- "Skip for now" closes the first-run gate permanently and says so: the button
  asks for confirmation ("This page won't come back.") and the page points at
  Settings → Bot connection to finish later.
- Security: no secrets written to DB; `.env` chmod 600. Rate-limiting the setup endpoint: ⏳ future (not yet implemented — `/setup` is gated only by the setup-done flag).

## Phase 2 — Setup-mode runtime (main.rs)
- Boot DB and web server even when unconfigured.
- Bot/dispatcher/background processors (invoice verification, scheduled posts) start only
  when `cfg.is_configured()`; otherwise a placeholder bot is used and a notice is logged.
- Web port from `cfg.web_port()`.
- Owner account seeding skipped in setup mode (wizard creates it).

## Phase 3 — Docker one-command deploy
- `Dockerfile`: multi-stage (rust:1.98 builder -> debian:bookworm-slim runtime), copies binary + schema.sql, exposes 8080.
- `docker-compose.yml`: builds image, maps `${WEB_PORT:-8080}:8080`, mounts `./data` and `./.env`,
  env_file optional, restart unless-stopped, healthcheck.
- First run: `docker compose up -d --build`, then open wizard at http://localhost:8080/setup.

## Phase 4 — setup.sh
- Interactive prompts with validation (bot token validated via curl getMe, owner ID numeric).
- Writes `.env` (mode 600), creates data dir.
- Optional: cargo build --release and/or docker compose up -d --build.

## Phase 5 — Hardening & UX (partially shipped in v1.3.x)
- Encrypt secrets at rest in DB if env cannot be used (AES-256-GCM with key from `MASTER_KEY`
  env/volume), never plaintext. ⏳ future.
- Auto-restart after wizard (process exit code 0 under Docker/systemd). ⏳ future.
- In-app "Settings" page to rotate tokens (rewrites .env + prompts restart). ✅ shipped
  (`/settings`, owner-only; bot token write-only, owner ID, default channel, Crypto Pay token,
  dashboard password; reuses the setup env writer). Help copy is plain language (no
  file-permission jargon), a collapsed "How to get a bot token" recipe covers a lost or
  revoked token, and "Push commands & profile to Telegram" re-pushes the bot profile
  after profile changes.
- Backup/restore helper for `data/` volume. ✅ shipped (`scripts/backup.sh`, `make backup`,
  WAL-safe `sqlite3 .backup`).
- Skippable setup + owner claim mode + managed channels (v1.3.x). ✅ shipped
  (see `PLAN.md` v1.3.x).

## Run more than one bot

Not a phase — the shipped multi-bot path (full guide: `docs/MULTI_BOT.md`).
Point `BOTS_CONFIG` at a TOML file and keep each token in its own env var:

```toml
[[bots]]
key = "primary"
token_env = "PRIMARY_BOT_TOKEN"
owner_id = 123456789
default_channel = "@main_channel"

[[bots]]
key = "shop2"
token_env = "SHOP2_BOT_TOKEN"
owner_id = 987654321
```

Then `export PRIMARY_BOT_TOKEN=...`, `export SHOP2_BOT_TOKEN=...`,
`export BOTS_CONFIG=/path/to/bots.toml` and **restart** — the bot list and the
tokens are read only at startup. The legacy `TELEGRAM_BOT_TOKEN`/`OWNER_ID` pair
still works and becomes the bot keyed `primary`; tokens are never written to the
database, and the console `/bots` page edits only owner, default channel and
enabled.

## Concrete file changes
- `src/config.rs`: `from_env()` -> `Self` (no hard failure), `is_configured()`, defaults, helpers.
- `src/main.rs`: setup-mode boot, conditional bot/dispatcher/background tasks, `cfg.web_port()`.
- `src/web.rs` (now `src/web/` — the setup wizard lives in `src/web/channel_ops.rs`): `/setup` GET/POST, redirects in setup mode, `validate_bot_token()`.
- New: `Dockerfile`, `docker-compose.yml`, `setup.sh`, `.env.example` (updated), `docs/EASY_SETUP.md`.

## Security notes
- Secrets (bot token, crypto token, dashboard password) are written only to `.env` (mode 0600)
  or environment; never stored in SQLite.
- Bot token validated before persisting; owner ID validated as positive integer.
- Setup endpoint should be disabled once configured (`is_configured()` guard).
- Single binary deployment preserved; no new runtime dependencies beyond reqwest/curl.
