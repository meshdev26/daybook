# Changelog — Daybook

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/);
versioning tracks the milestone: milestone vN ships as Cargo `0.N.0`
(see [`docs/RELEASING.md`](docs/RELEASING.md)).

## [0.4.1-alpha]

Alpha pre-release of the v1.4 milestone. Not independently audited; see the
notice in `README.md` and the pre-real-money checklist in issue #1.

### Changed
- Published name is `daybook` across tags, Cargo metadata and release artifacts.
- Recovery codes use the BIP-39 wordlist and respect `MFA_RECOVERY_CODES_ENABLED`.
- Dashboard password DM fallback is documented honestly; set `WEB_URL` for one-time links.
- MCP tools are fully enumerated: 38 across Read/Write/Sensitive tiers.
- `cookie` upgraded to 0.18.3 (RUSTSEC-2026-0594).
- Private vulnerability reporting enabled; promotion-blocking doc contradictions fixed.

## [Unreleased]

The 2026-10-27 hardening wave plus the v1.4 milestone
(`docs/ARCHITECTURE-V1.4.md`: bookings, MCP per-seller tokens, setup links),
and the setup & management UX polish
(`design-system/daybook/SETUP_UX_POLISH_SPEC.md`). Not yet tagged.

### Added
- At-rest secret encryption: `MASTER_KEY` (32 bytes as hex or base64) seals sensitive config values with AES-256-GCM into versioned `enc:v1:` envelopes. A set-but-invalid key aborts startup; without a key, secret writes are refused with a clear console error. Legacy plaintext values stay readable and migrate on re-save. Startup logs only whether encryption is active.
- Deterministic webhook-to-grant integration test: signed webhook, atomic claim, provider stub verification, exactly-once subscription and replay protection, all against local mocks.
- `MFA_RECOVERY_CODES_ENABLED` (default on): `0`, `false`, `no` or `off` rejects stored recovery codes, removes the recovery route and console UI, and leaves TOTP sign-in working. Stored codes stay in the database and revive if the flag is re-enabled.
- Recovery codes now draw from the vetted BIP-39 English wordlist: 10 one-time four-word, hyphen-joined codes, bcrypt-hashed at rest. The UI states they are not wallet seed phrases.
- Bookings domain closed end-to-end: console `/bookings` page (seller-scope
  table, status filter, confirm/decline/cancel actions with audit, read-only
  services panel), buyer `/mybookings` and `/cancelbooking`, seller
  `/bookingconfirm` and `/bookingdecline` (admins/owners respond on the
  console), atomic slot claims — one `BEGIN IMMEDIATE` transaction decides
  every `/book` — and held-slot sweeper DMs to buyers when a hold expires.
- MCP per-seller tokens: bearer auth wired into the MCP endpoint (SHA-256
  hash lookup after the owner env token, identical 401 for unknown/revoked/
  expired), self-service via `/mcptoken create|list|revoke` (raw token shown
  once, prefix-masked list, 10 active tokens per seller), and per-seller
  principals scoped to their own data on the tools.
- One-time setup links: `/setup/claim` (15-minute, single-use, auto-login)
  replaces DM'd plaintext passwords for `/becomeseller` and `/dashboard`.
- Revenue-by-asset analytics on the console Analytics page — per-asset rows
  per the finance ruling (cross-asset sum display removed).
- Trust pack: `CHANGELOG.md` (Keep a Changelog) and `docs/RELEASING.md`
  release runbook; Cargo tracks milestones (`0.N.0` for milestone vN).
- Owner money approvals in Telegram: `/refundapprove` and `/payoutapprove`
  (admin+) — approve-only; execution stays on the console Finance page or the
  REST execute endpoint.
- Console `/finance` page: payout and refund queues with create/approve/execute
  and explicit ambiguous-failure banners.
- Console `/keys` page: API key management with one-time reveal; AI settings
  templates panel.
- Seller AI-settings endpoints (`GET/POST /sellers/{user_id}/ai-settings`) and
  code-redemptions endpoint (`GET /codes/{code}/redemptions`).
- Mandatory TOTP enrollment with embedded QR (vendored `qrcodegen`, MIT) and a
  copyable `otpauth` URI.
- Password show/hide toggles on all secret inputs.
- Multi-bot console: a Bot selector in the sidebar rail for the global owner
  (`POST /bot/select`, persisted per session, unknown/disabled bots rejected
  server-side; sellers and admins get a pinned label instead) and an
  owner-only `/bots` page editing bot metadata — owner, default channel,
  enabled. Tokens stay in `BOTS_CONFIG`/`BOTS_JSON`/env; the page never reads
  or edits them.
- Bot-scoped console reads for a scoped owner: plans, products, orders,
  channels, dashboard, codes, reviews, analytics and finance filter to the
  selected bot's sellers (or channels); "All bots (primary)" is the unscoped
  reset.
- Owner-only `bot_key`/`bot_id` selectors on the MCP `publish_post`,
  `broadcast` and `ai_publish_post` tools; per-seller MCP tokens ignore them
  and act through their own seller's bot.
- MCP `publish_post`/`edit_post` accept an optional `parse_mode` (`"html"`
  only): the send carries Telegram HTML entities; absent keeps the
  historical plain-text path with zero drift for scripted callers.
- Scheduled-post management from the bot: `/scheduled` lists pending posts
  (id, scheduled UTC time, text preview) with the `/orders` scoping
  convention (sellers see their own, admins/owner all), and
  `/cancelpost <id>` cancels one before it fires (accepts `#id`; scoped to
  the caller, unknown/already-sent ids share one refusal). Cancellation is
  a new `cancelled` status; the due loop and the console's post pages are
  unaffected.
- Startup delivery self-check: each started bot DMs its owner once at boot
  ("connected and polling — DM delivery works") after resolving its own
  username via `get_me`. A failed DM or `get_me` is logged loudly — "the
  bot can't DM me" is now diagnosable at boot — and never aborts startup.

### Removed
- Removed the separate AI-agent workflow layer; AGENT.md is now the only agent instruction file.

### Changed
- Cross-asset money totals are no longer returned as a single summed float. Revenue is reported as per-asset lines in the REST/MCP analytics and finance responses and on the console Analytics page; a clearly labeled marker replaces a numeric total when more than one asset is present. This is a breaking response-shape change for clients that read `revenue_total` as a number.
- Setup and management UX polish (per
  `design-system/daybook/SETUP_UX_POLISH_SPEC.md`): the `/setup`
  wizard groups the form into "Your bot" and "Your console login" with the
  optional fields collapsed under "Optional — add later in Settings",
  validates locally before contacting Telegram, re-renders errors in place
  with the typed values preserved (token and password are re-entered for
  safety), asks before "Skip for now" ("This page won't come back."), and its
  success page separates what works now (console login) from what needs the
  restart (the bot), with per-platform restart commands. Settings copy is
  plain language (no "0600 permissions" jargon) and gains a collapsed "How to
  get a bot token" recipe, the renamed "Push commands & profile to Telegram"
  action, and distinct labels on the two MFA code inputs. `/bots/add` becomes
  a validate-then-configure two-step wizard whose fallback is a copy-paste
  snippet panel instead of a raw file download; `/bots` save/disable/delete
  confirm in plain language and every bot mutation flags "restart required",
  so the dashboard restart banner actually appears after bot changes.
- Bot-aware sends and background work: broadcasts, invite links, `/setupbot`
  profile updates, console posts, REST sends, and the scheduled-post, expiry
  and held-booking sweepers resolve the owning bot per seller (`for_seller`)
  instead of always sending through the primary bot.
- Docs: new `docs/MULTI_BOT.md` (operator guide); Bot Switching UX spec raised
  to v1.1 with the scoping table extended to dashboard, codes, reviews,
  analytics and finance.
- `/book` tells the truth: the reply says the booking is **requested — not
  confirmed**, with a 60-minute seller-response window (the hold is the
  response window; an expired hold can still be confirmed while the slot is
  free).
- `/slotadd` validates datetimes strictly (RFC3339 or `YYYY-MM-DD HH:MM`,
  future-only) and clamps `capacity` to 1 — one slot holds exactly one
  booking (class capacity deferred to v1.5).
- `/bookings` in the bot points buyers at `/mybookings`; `/becomeseller` and
  `/dashboard` DM a one-time setup link instead of a plaintext password
  (plaintext fallback only while `WEB_URL` is unset).
- Buyer shop listings show seller names; invoice messages carry an Amount line
  and a Pay-now button; buyer copy states what actually happened.
- Role-aware `/start`, `/menu`, `/help`; `/book` and `/services` opened to the
  buyer tier.
- Docs truth-synced with the code: `docs/API.md`, `docs/COMMANDS.md`,
  `docs/SECURITY.md`; `.env.example` drops removed webhook vars and documents
  `AI_*`.
- Build/CI: toolchain pinned at 1.99.1 across CI and Docker; `build.rs` fails
  loudly on a missing Windows icon; `make restore` added; doctor probes are
  base-path aware.
- Bot transport budgets: every bot shares one reqwest client (30 s long poll,
  45 s total, 10 s connect) wired via `Bot::with_client`, with an explicit
  `Polling` listener (30 s hold per cycle) and dispatcher-death logging — a
  wedged call is cut off and retried by the backoff instead of leaving the
  bot deaf on high-latency routes. Pinned by a loopback black-hole test.

### Fixed
- Unauthorized bot commands no longer fall into a silent `return` — the bot
  answers with one short neutral line ("This account isn't authorized for
  management commands on this bot.") and leaks no role names; all role
  logic is unchanged.
- Bots skipped at startup (unset/empty token env var, `owner_id` 0, or
  console-disabled) are now logged per bot — key plus `token_env` name,
  never the token — and a zero-bot boot logs one loud configured-vs-started
  summary instead of silently polling nothing.
- Scheduled posts with an unparseable `scheduled_at` (e.g. a web-submitted
  raw string) can no longer sit pending forever: the due loop compares the
  stored string lexicographically, so such a row either fired at the wrong
  time or never fired; a per-cycle sweep now logs the row id and raw value
  at error and routes it into the bounded retry → terminal-failed path.
- `/schedule` refuses a datetime it cannot parse ("use YYYY-MM-DD HH:MM;
  times are UTC") and stores nothing, instead of writing the raw string as
  a row that never fires. The lenient formats are unchanged.
- Broadcast failures are visible: each undeliverable recipient is logged at
  warn (user id + error, no payload) with an attempted/sent/failed summary
  at info; the expiry sweeper logs failed kick/unban calls at debug
  (previously fully swallowed), and failed `/approvals`, `/approve`, `/deny`
  replies log the approval id at warn.
- The bot's owner gets one discoverability tip when sending plain text in
  an idle chat (`/post` publishes, `/schedule` queues, UTC); buyers, other
  roles and non-text updates stay silent as before.
- Invoice claim/release lifecycle covers products (previously invoices only):
  every failure path releases the claim; stale invoices requeue after 5 minutes
  instead of granting.
- `/expirecheck` DMs buyers whose subscriptions expired; invite failure and
  no-channel states release the held claim.
- Setup/Settings `.env` writes are atomic (temp file 0600 + rename) and
  preserve unknown keys.
- Bot add wizard: on `toml` 1.x the config-file write's parse check used a
  parser that only accepts bare values, so it always failed and validated
  setups silently fell back to the manual copy-paste snippet. TOML writes now
  validate with the document parser and land in `bots.toml` directly.
- Crypto Pay client requests time out after 15s.
- P1: live dispatch stack overflow — tokio worker stacks raised to 16 MiB
  (runtime semantics otherwise unchanged); two production crashes during
  real-update dptree dispatch (deepest frame chain) stopped reproducibly.

### Security
- MCP Sensitive tier is fail-closed owner-only: a per-seller token calling a
  Sensitive tool gets JSON-RPC `-32001 "sensitive tools require the owner
  credential"` before anything parks an approval or validates arguments — the
  requester-blind `/approve` flow stays owner-only by construction.
- Setup-link claims are login-class: the POST shares the sign-in throttle,
  tokens are 48-char CSPRNG values stored SHA-256-hashed, single-use
  (`DELETE … RETURNING` — exactly one claim can win), and a successful claim
  signs out every other session of the account. No raw token is ever logged.
- REST MFA parity: `POST /auth/mfa/verify` pending-token exchange; privileged
  accounts must complete TOTP on login, matching the web console.
- CAS execution guards for payouts/refunds: `approved → executing →
  paid/failed` with ambiguous-failure semantics and attempt/ambiguous audit
  events; the ungated `orders_refund` endpoint removed.
- Channel proof-of-control: `/setchannel` and `/setup` verify the seller
  administers the target channel (`getChatAdministrators`); `[@channel]`
  posting commands resolve to the seller's own channel only.
- Security-headers middleware on every response (CSP, `nosniff`,
  frame-ancestors DENY, no-referrer, Permissions-Policy).
- WCAG 2.1 AA floor in the console (release floor):
  contrast tokens, skip link, labelled controls, visible focus, alert roles.
- Output escaping applied across all new console surfaces (settings values, AI
  settings, dynamic rows) and client-side rendering in `assets/app.js`.
- Self-benefiting payout refusal: owner/admin cannot approve a payout to
  themselves.

## [1.3.x] - 2026-10-26

Untagged milestone wave (v1.3 / v1.3.x per `PLAN.md`); spans
2026-10-25 → 2026-10-26 after the v1.2.1 tag.

### Added
- MCP server for AI agents: `POST /api/v1/mcp` (JSON-RPC, zero new
  dependencies) with tiered tools — read (PII and codes masked), write
  (audited), sensitive (parked behind owner approval via `/approvals`,
  `/approve`, `/deny`, 15-minute expiry); every call and approval lands in
  `mcp_audit`; endpoint disabled unless `MCP_OWNER_TOKEN` is set.
- Skippable setup: `/setup` gains "Skip for now"; with setup closed and no
  owner account, `/login` claims the console — loopback claims freely, remote
  requests need the one-time claim code printed at startup.
- Console `/settings` (owner-only): bot token (write-only, getMe-validated
  before write), owner ID, default channel, Crypto Pay token, dashboard
  password rotation with other-session invalidation.
- Channels become managed data: global channel manager in the console (add /
  set default / remove), per-seller read-only view, channel dropdown on the
  Post form.
- Orders-lite: physical purchases create `orders` rows; `/orders` and
  `/orderset <order_id> <status> [tracking]`; console Orders page.
- `send_document` delivery: `digital_file` products re-send the actual
  Telegram document when the payload is a `file_id`.
- Prometheus `/metrics` endpoint (requests by route template, MCP tool calls,
  bot messages sent, uptime); `/health` reports db/bot/mcp/uptime honestly.
- The operator's ledger design system: DESIGN.md tokens materialized, light +
  dark themes, bundled IBM Plex fonts (CDN dependency removed), ledger tables,
  badge system, guided empty states, responsive rail, reduced-motion support.
- 30-second request-timeout middleware.

### Changed
- `auth_password` verifies the current password for non-owners before rotating
  and invalidates every other session; `codes_delete` is seller-scoped
  (cross-seller deletes get 403, redemption rows included).
- Reliability: poisoned-mutex recovery instead of panic (122 sites), bcrypt off
  the async runtime, WAL checkpoint on shutdown, invoice-verifier and
  scheduled-post retry counters with a terminal failed state, systemd unit
  gains `TasksMax` + reasoned `ReadWritePaths`.

### Fixed
- CI runs the project gate (`make test`) on every push and pull request.
- The checks pass on a clean checkout; clippy and rustfmt ship in the toolchain pin.
- The patched teloxide fork is shipped in-tree, so the repo builds on any
  machine while keeping the RUSTSEC-2026-0173 subtree out of the lock.
- Documented that a real environment variable wins over `.env` (systemd and
  desktop launches have a clean environment, where the file is authoritative).

### Security
- Plan resolution is seller-scoped: own namespace wins, global is
  admin/owner-only, ambiguity errors, plain buyers see no global plans —
  closes the cross-seller pricing leak and `/buy` mis-resolution found in the
  2026-10-25 review follow-up; legacy databases are deduped before the
  enforcing index.

## [1.2.1] - 2026-10-25

Initial public release (tag `v1.2.1`).

### Added
- Channel subscriptions/memberships, activation codes, one-time digital
  products (link/file/text, auto-delivered), physical products with order
  tracking.
- Refunds/payouts with owner-approved Crypto Pay transfers and audit.
- One Rust binary, many profiles: desktop (.exe/.app/tar.gz), existing-server
  installer with hardened systemd unit, Docker, subpath mounting
  (`WEB_BASE_PATH`), embeddable dashboard (`embed::AgentAppBuilder`),
  compile-time command registry for extensions.
- 94 tests including property invariants and libFuzzer targets; WAL-safe
  `make backup`; `make doctor` diagnostics.

### Security
- Same-day security floor (2026-10-25 review remediation): CSPRNG tokens with
  one-time session/API-key rotation; `/setup` gated by DB `setup_done`;
  mandatory constant-time webhook signatures; atomic fail-closed payment
  claims; seller channel scoping enforced; XSS escaping; shared login throttle
  (web+api); `MAX_PLAN_DAYS` clamp with checked date math.
