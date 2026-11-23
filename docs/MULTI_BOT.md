# Running more than one bot

One process, one database, one console — several Telegram bot tokens.

Each configured bot has its own key, its own owner (Telegram user id) and an
optional default channel. Every bot answers its own updates: a dispatcher is
started per bot at boot, so `/start` sent to bot B is handled by bot B.

**Business data stays seller-scoped.** A bot does not own plans, products,
orders or subscriptions — a *seller* does. Sellers (and channels, and admins)
carry a `bot_id` that resolves which bot acts for them; sellers with no
`bot_id` are attributed to the primary bot (the first bot in your config). This
is why adding a bot never duplicates data: it changes which token sends, not
which rows exist.

## Configuration

Bot tokens are read **at startup** from the environment or a config file. They
must never be committed.

### Option 1 — `BOTS_CONFIG` (TOML file)

```toml
# bots.toml — referenced by BOTS_CONFIG=/etc/daybook/bots.toml
[[bots]]
key = "primary"
name = "Primary"
token_env = "PRIMARY_BOT_TOKEN"   # name of the env var, never the token
owner_id = 123456789
default_channel = "@my_main_channel"   # optional

[[bots]]
key = "shop2"
name = "Second shop"
token_env = "SHOP2_BOT_TOKEN"
owner_id = 987654321
default_channel = "-1001234567890"     # @name or numeric chat id
```

```bash
export PRIMARY_BOT_TOKEN="..."   # in .env, a systemd EnvironmentFile, or the shell
export SHOP2_BOT_TOKEN="..."
export BOTS_CONFIG=/etc/daybook/bots.toml
```

`token_env` is the recommended form: the TOML file then contains no secret and
can be checked into version control or a config volume. (The parser also
accepts an inline `token` key; avoid it — the file would hold the secret.)

### Option 2 — `BOTS_JSON` (inline array)

Same fields, as a JSON array in one environment variable:

```bash
BOTS_JSON='[{"key":"primary","name":"Primary","token_env":"PRIMARY_BOT_TOKEN","owner_id":123456789,"default_channel":"@main"},
            {"key":"shop2","token_env":"SHOP2_BOT_TOKEN","owner_id":987654321}]'
```

### Option 3 — legacy single bot (unchanged)

`TELEGRAM_BOT_TOKEN` + `OWNER_ID` + optional `DEFAULT_CHANNEL` still work
exactly as before and are synthesized into one bot with the key `primary`.
Nothing changes for existing installs.

**Precedence:** `BOTS_CONFIG` (if it reads and parses to at least one bot) →
`BOTS_JSON` → the legacy env pair. A file that is missing, unparsable or empty
falls through rather than starting with zero bots.

**Validation:** an entry whose token env var is unset/empty, or whose
`owner_id` is `0`, is skipped — that bot simply is not polled. Keep the
`owner_id` of each bot equal to the person who should control it.

### Restart required

The bot list is resolved once at boot. Adding, removing, re-tokenizing or
re-enabling a bot takes effect on the **next restart**; there is no hot reload.
`bots.toml` and the env vars are the source of truth for tokens.

### Token hygiene

- Tokens live in the environment / `bots.toml` only. They are **never written to
  the database** — the `bots` table stores key, owner id, default channel and an
  enabled flag, nothing secret.
- The console (below) can therefore never display or edit a token — the most
  it ever echoes is the last four characters as a hint on the Bots page.
  Pasted tokens are validated and stored, never rendered back.
- Treat `bots.toml` as config, and the env file as `0600` secret material, the
  same as `TELEGRAM_BOT_TOKEN` today.

## Console

The operator console has one login and one URL. Scope is chosen inside it.

### Owner

The sidebar rail carries a **Bot** selector listing `All bots (primary)` plus
every *enabled* bot. Choosing one persists against your session and narrows the
pages you read; choosing `All bots (primary)` resets to the unscoped global
view (it is a reset, not a filter on the literal primary bot). A crafted
`bot_id` naming an unknown or disabled bot is not honored — validation drops it
and the session stays unscoped.

### Sellers and admins (pinned)

Sellers and admins see a plain pinned label naming the bot's key instead of a
selector: the bot that already resolves their account (seller →
`sellers.bot_id`, admin → `admins.bot_id`, otherwise primary). They cannot
switch, and a forged `POST /bot/select` from their session is refused.

### What the scope changes (read filter only)

| Surface | Scoped by |
|---|---|
| `/plans`, `/products`, `/orders` | sellers belonging to the selected bot |
| `/channels` | channels that resolve to the selected bot |
| `/dashboard` | plans and recent subscriptions of that bot's sellers; the default-channel readout resolves the selected bot's channel (global value under "All bots") |
| `/codes` | code rows and the plan picker |
| `/reviews` | products of that bot's sellers |
| `/analytics` | aggregate over that bot's sellers (global aggregate under "All bots") |
| `/finance` | payout and refund lists |

Scoping narrows **reads only**. Every POST keeps its existing authorization:
being scoped to one bot does not grant or remove any privilege.

Sends, however, follow the resolved bot — an owner-scoped console operates
**as** the selected bot for broadcasts, invite links, `/setupbot` profile
updates, post creation, and the REST calls behind the console.

### Bots page (`/bots`, global owner only)

One card per bot. A card edits the bot's **owner user id, default channel id
and enabled flag**, with **Disable** (reversible) and **Delete** actions
beneath the save form. Every action speaks plainly and honestly: confirms name
the bot by its key and state the consequence, a successful save answers "Bot
settings saved — they apply on restart.", a failed save re-renders the page
with an error and changes nothing, and each mutation sets the restart-required
flag that shows the restart banner on the dashboard and Settings. Deleting
removes the bot's settings from the console; bots that come from your config
file reappear on restart unless you remove them from the file.

**Add bot** (`/bots/add`) is a two-step wizard. Step 1 validates the token
against Telegram (`getMe`); step 2 — nickname, name, owner user id, default
channel, and the env-var name that will hold the token — only unlocks once the
token validates, and comes pre-filled from the validated bot (username,
suggested nickname, suggested token-env name). If the console cannot write
your bots config file, it shows a copy-paste `[[bots]]` snippet with two-step
manual instructions (create `bots.toml`, point `BOTS_CONFIG` at it, restart)
instead of failing silently. Token editing stays out of scope by design and
the page says so.

Two honest notes:

- The page is reached from the owner-only **Bots** entry in the console's
  **Operate** group (after Products). It stays owner-only: sellers and admins
  never see the entry and the route rejects them.
- The dispatcher's owner id and the polled bot set are built from the startup
  config, and the DB `bots` rows are boot-loaded too, so an edited owner (or a
  disabled bot) only takes effect after a restart.

## MCP

There is a single MCP endpoint, `POST /api/v1/mcp`, for all bots — there are no
per-bot endpoints.

- The **owner token** (`MCP_OWNER_TOKEN`) may pass optional `bot_key` or
  `bot_id` on the Telegram-acting tools (`publish_post`, `broadcast`,
  `ai_publish_post`). `bot` is accepted as a string alias of `bot_key`. With no
  selector the call acts as the **primary** bot. An unknown or disabled bot is
  an error; selectors naming different bots are an error.
- **Per-seller MCP tokens** ignore the selectors entirely and always act as the
  bot mapped to their seller id (primary when unmapped) — a valid `bot_id` can
  never reach another tenant's bot.
- Sensitive tools (`broadcast`, `ai_publish_post`) keep their approval flow: the
  selectors travel with the parked approval and apply when it is executed.

## Limitations (by design, today)

- **No hot add/remove.** Editing the bot list or a token requires a restart.
- **No encrypted token storage.** Tokens are not stored in the database at all;
  there is no console or DB path to manage them.
- **No per-bot MCP endpoints.** One endpoint, optional owner-only selectors.
- **No console path to scope an admin to a bot.** `Db::set_admin_bot_id` exists
  in the data layer, but no page calls it (planned v2). Until then an admin
  resolves `admins.bot_id` when set and the primary bot otherwise, and admin
  page data stays global — pinning affects their *sends*, not their reads.
- **No "one task per bot" background jobs.** The scheduled-post, expiry and
  held-booking sweepers run once for the process and resolve the sending bot
  per row (`for_seller`), so a bot never DMs or kicks for another bot's seller.

## See also

- `design-system/daybook/BOT_SWITCHING_UX_SPEC.md` — the UX spec of
  record (scoping table, accessibility floor).
- `docs/reviews/2026-10-27-bot-scoping-security-review.md` — security review and
  accepted notes.
- `docs/COMMANDS.md`, `docs/SETUP.md` — per-command and first-run setup.
