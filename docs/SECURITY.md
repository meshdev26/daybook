# Security Model

## Authentication
- Telegram: owner/admin gating via OWNER_ID + admins table.
- Web: password (WEB_PASSWORD) + SHA-256 session cookie.
- Web MFA: TOTP (RFC 6238) enrollment is mandatory for owner/admin web accounts at first login (QR code plus a copyable `otpauth://` URI for manual entry). Code verification is throttled (5 attempts per 15 min); a pending MFA challenge cookie lives 5 minutes. Recovery codes: 10, bcrypt-hashed at rest, shown once at issuance and regenerable from Settings; the owner can reset a user's MFA from the Web users page. Plaintext secrets are never rendered after enrollment. Disabling MFA or regenerating recovery codes requires the current second factor (a valid TOTP code, or an unused recovery code, which is consumed).

## Authorization
- Public commands: start, help, menu, plans, buy, paid, redeem, setup, becomeseller.
- Admin commands: require `is_admin`.

## Payments
- Always verify payment via Crypto Pay API before granting access.
- Never trust client-side payment claims.

## Secrets
- All tokens in `.env` (gitignored).
- No secrets in code or logs.

## CSRF
- Web dashboard relies on `SameSite=Strict` session cookies for CSRF protection; no per-form CSRF tokens are emitted. If a future requirement (e.g. L10) demands token-based CSRF, implement hidden-field tokens in all POST forms.
- All POST forms are cookie-authenticated; SameSite=Strict prevents cross-site submission in modern browsers.

## Secrets at rest (planned)
- Future: encrypt sensitive columns (tokens, secrets) at rest with AES-256-GCM keyed by a `MASTER_KEY` env var. Not yet implemented; currently relies on filesystem permissions (SQLite file, `.env` mode 600).

## MCP tokens (v2, planned)
- Per-seller bearer tokens will live in the `mcp_tokens` table (schema.sql + migration), scoped by `seller_id`, with `McpToken` lookup in `src/mcp/mod.rs`. Not yet wired into `authenticate`; `MCP_OWNER_TOKEN` remains the only live credential.

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
- **REST MFA parity:** a correct password alone never mints a REST session for a TOTP-enabled account — `POST /api/v1/auth/login` returns a 5-minute single-use pending token, exchanged (with a TOTP or recovery code) at `POST /api/v1/auth/mfa/verify` for the session; failed verifies are throttled 5 per 15 min per pending token. Privileged roles without TOTP enrolled get 401 `mfa_required`; enrollment remains web-console-only.
- **Console accessibility floor (WCAG 2.1 AA):** CSS text tokens held at >= 4.5:1 contrast on their surfaces, labelled form controls and icon buttons, and a skip-to-content link.
