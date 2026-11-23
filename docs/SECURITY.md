# Security Model

## Authentication
- Telegram: owner/admin gating via OWNER_ID + admins table.
- Web: password (WEB_PASSWORD) + SHA-256 session cookie.
- Web MFA: TOTP (RFC 6238) enrollment is mandatory for owner/admin web accounts at first login (QR code plus a copyable `otpauth://` URI for manual entry). Code verification is throttled (5 attempts per 15 min); a pending MFA challenge cookie lives 5 minutes. Recovery codes: 10 one-time codes drawn from the BIP-39 English wordlist, bcrypt-hashed at rest, shown once at issuance and regenerable from Settings; they are not wallet seeds and are never valid in a crypto wallet. `MFA_RECOVERY_CODES_ENABLED` defaults on; `0`, `false`, `no` or `off` rejects stored codes, hides the recovery UI and unregisters its route while TOTP verification stays live. Stored codes are preserved and work again if the flag is re-enabled. Disabling the feature without a working authenticator can lock an account out; the owner can reset a user's MFA from the Web users page. Plaintext secrets are never rendered after enrollment. Disabling MFA or regenerating recovery codes requires the current second factor (a valid TOTP code, or an unused recovery code, which is consumed when the flag is on).

## Authorization
- Public commands: start, help, menu, plans, shop, buy, paid, redeem, services, book, bookings, mybookings, cancelbooking, setup, becomeseller.
- Seller commands: channel/plan/product/code/booking/crypto/AI surfaces and `/mcptoken`; each scoped to the caller's own data (admins see all).
- Admin commands: require `is_admin`.

## Multi-tenant isolation
- Every business row carries `seller_id`; all reads and writes go through `Db` methods.
- Owner/admin are the global privileged tier; sellers see only their own rows. A forged `seller_id` in a request is rejected 403.
- Bot scoping: `sellers.bot_id` picks the seller's bot (NULL falls back to the primary); web and REST handlers resolve it through `effective_bot`.
- MCP per-seller tokens map to `Principal { role: Seller, seller_id: Some(seller_id) }`, scoped to that seller's data.
- Sessions carry `seller_id`; a seller cannot switch bots from the console (the bot is pinned server-side and shown as a label).
- Role and tenant gates live in handlers and `Db`, not just in navigation.
- Gap: independent multi-tenant penetration testing has not been run. Validate isolation on your own deployment before handling real customer money.

> **Status:** alpha, self-reviewed (with AI assistance), not independently audited; no real customer money. Gap checklist: https://github.com/meshdev26/daybook/issues/1.

## Payments
- Always verify payment via Crypto Pay API before granting access.
- Never trust client-side payment claims.

## Secrets
- All tokens in `.env` (gitignored).
- No secrets in code or logs.

## CSRF
- Web dashboard relies on `SameSite=Strict` session cookies for CSRF protection; no per-form CSRF tokens are emitted. If a future security requirement demands token-based CSRF, implement hidden-field tokens in all POST forms.
- All POST forms are cookie-authenticated; SameSite=Strict prevents cross-site submission in modern browsers.

## Secrets at rest (implemented)
- Sensitive config keys (currently `crypto_token`) are sealed with AES-256-GCM before they are written to the SQLite `config` table. `MASTER_KEY` is a 32-byte key supplied as 64 hex characters or standard base64; a set-but-invalid value aborts startup with a clear error.
- Stored shape: `enc:v1:<base64(nonce || ciphertext || tag)>`, with a fresh 12-byte OS-random nonce per write. Authentication failures (tampering, corruption, wrong key) are errors, never plaintext. The key and plaintext are never logged.
- Behavior change: with no valid `MASTER_KEY`, secret writes are refused and the Crypto console form shows an error. The operator must set the key before saving a token. Reads of legacy unprefixed values still work, so existing deployments keep running; re-save each secret once to migrate it to an encrypted envelope.
- Non-secret config keys keep verbatim storage and unchanged behavior. Startup logs only whether secret encryption is active, never the key.

## MCP tokens (v1.4, live)
- Per-seller bearer tokens live in the `mcp_tokens` table, scoped by `seller_id`, stored SHA-256-hashed with an 8-char display prefix (raw shown once at mint). `authenticate` in `src/mcp/mod.rs` tries the owner env token first, then the hash lookup; revoked/expired rows never authenticate and every failure returns the same 401. Minted/revoked by the seller via `/mcptoken` (10 active max). See the 2026-10-27 additions below for the Sensitive-tier owner-only gate.

## Data
- SQLite file permissions; back up regularly.
- Least-privilege bot admin rights.

---

## Security guarantees as of v1.2.1 (2026-10-25 remediation)

- **Tokens:** all sessions/API keys/codes generated from the OS CSPRNG (`getrandom`); the previous clock-seeded LCG was removed and a one-time migration invalidated every credential issued before the fix.
- **Payments fail closed:** no grant without an explicit `status=="paid"` from the Crypto Pay API; verification errors or a missing token never grant. Webhook signatures are mandatory (constant-time compare).
- **Exactly-once grants:** invoice claiming is an atomic conditional UPDATE; `UNIQUE(invoice_id)` on subscriptions and product grants makes double-delivery structurally impossible (property-tested, fuzzed).
- **Setup is one-shot:** first-run `/setup` closes against a DB flag the moment configuration completes — no replay window.
- **Multi-tenancy:** seller scoping on plan/product/order/finance paths; seller channel fallbacks can no longer reach the global default channel; manual "paid" transitions rejected.
- **Credentials hygiene:** `.env` mode 600, secrets never logged, never in process argv (doctor uses `curl -K`), installer never prints env values.
- **Rate limiting:** shared web+API login throttle (5 failures / 15 min, bounded memory).
- **Money movement:** refunds/payouts require owner approval before any transfer (human-in-the-loop); every transition audited.
- Review + remediation record: `docs/reviews/2026-10-25-architecture.md`; independent penetration testing is recommended before high-volume operation.

---

## Additions as of 2026-10-27 (remediation wave)

- **Security headers:** middleware on every response — `Content-Security-Policy` (`default-src 'self'`; script/style still allow `'unsafe-inline'` as an explicit interim step — a nonce-based CSP is the follow-up), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, and `Permissions-Policy` disabling camera/microphone/geolocation.
- **CAS execution guard for money transfers:** payout/refund execution first atomically claims the row `approved → 'executing'` (a losing concurrent executor gets 409 and never transfers), then runs the Crypto Pay transfer. A definite API rejection reverts the row to `approved` and returns 502 `transfer_failed` (retryable); an ambiguous failure (timeout/network/5xx — money state unknown) returns 503 `execution_ambiguous` and deliberately leaves the row `executing` for operator reconciliation on the Finance page. Every attempt is audited (`payout/refund_execute_attempt`, `payout/refund_execute_ambiguous`, plus executed/failed outcomes).
- **Channel binding proof of control:** non-admin callers of `POST /api/v1/sellers/{user_id}/channel` may bind only channels they control — a `@handle` where the calling seller is a chat administrator (verified live through the bot via `getChatAdministrators`) or a negative group/channel id; freeform names and a bot-not-in-channel target are rejected. Admins/owner are exempt (bootstrap path).
- **REST MFA parity:** a correct password alone never mints a REST session for a TOTP-enabled account — `POST /api/v1/auth/login` returns a 5-minute single-use pending token, exchanged (with a TOTP or, when enabled, a recovery code) at `POST /api/v1/auth/mfa/verify` for the session; failed verifies are throttled 5 per 15 min per pending token. With `MFA_RECOVERY_CODES_ENABLED` off, stored recovery codes are rejected with `invalid_code` and the error text names only the authenticator code; TOTP verifies unchanged. Privileged roles without TOTP enrolled get 401 `mfa_required`; enrollment remains web-console-only.
- **Console accessibility floor:** CSS text tokens held at >= 4.5:1 contrast on their surfaces, labelled form controls and icon buttons, and a skip-to-content link.
- **One-time setup links (only when `WEB_URL` is set):** setting `WEB_URL` to a publicly reachable dashboard URL makes `/becomeseller` and `/dashboard` DM a single-use claim link (`/setup/claim?token=…`) instead of a password. The token is a 48-character CSPRNG value, stored only as a SHA-256 hash with a 15-minute TTL, and consumed atomically on POST (`DELETE … RETURNING` — exactly one caller can win; expired, reused, unknown and wrong-purpose tokens all land on one generic no-oracle page). A successful claim sets the password, creates the claimant's session and signs out every other session of the account; the claim POST shares the login throttle (5 attempts / 15 min), and the raw token is never logged or echoed. Notes: the link holder can set a password without a second factor, and the token travels in the URL; the app never logs it, but reverse-proxy access logs containing `/setup/claim?token=` are an operator-side consideration (see `docs/RELEASING.md`).
- **Default fallback when `WEB_URL` is unset:** `WEB_URL` is commented out in `.env.example` and absent from `QUICKSTART.md`, so the default is the legacy path — `/becomeseller` and `/dashboard` DM a **plaintext dashboard password** over Telegram (and log a warning). Set `WEB_URL` to enable the one-time claim links above and stop DMing passwords.
- **MCP Sensitive tier is owner-only (fail-closed):** per-seller MCP tokens authenticate as seller principals scoped to their own data, but a Sensitive-tier tool call (`gen_codes`, `delete_post`, `broadcast`, `ai_publish_post`) from any non-owner principal is refused with JSON-RPC error `-32001 "sensitive tools require the owner credential"` before anything parks an approval or validates arguments — no side effects, audited as a seller-role failure. The gate is deliberate: the `/approve` flow is requester-blind (it executes whatever is parked), so the benefiting party must never be able to create a pending row; the owner's park → `/approve` → execute flow is unchanged.

**Vulnerability reporting:** report privately through GitHub Security Advisories — open the repository's **Security** tab and choose **Report a vulnerability**. Do not file a public issue for a vulnerability.
