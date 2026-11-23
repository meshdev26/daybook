# Setup Links & MCP Tokens — UX & Accessibility Spec

**Owner:** design — spec only; the web layer implements in `src/web.rs`.
**Status:** v1 spec. WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** web implementation, server handlers/data, core data layer, security review, design token contract.
**Related:** `ARCHITECTURE-V1.4.md` §3 (D6) and §4 (D7) — binding contracts, `assets/app.css`, `src/web.rs` (`claim_form_html` / `SECRET_TOGGLE` / `sign_in_form_html` idioms, `settings_password` password floor + session invalidation, `keys_body` shown-once pattern, `login_throttle`), `docs/API.md` (see §8.3 — the MCP section must exist for §7 copy to point at), `docs/EASY_SETUP.md` (docs notes the flow).

---

## 0. Summary of recommendations

1. **The DM carries no credential when claim links are enabled.** With `WEB_URL` set, `/becomeseller` and `/dashboard` stop DM-ing `gen_token(12)` passwords and DM exactly one thing: a single-use link, valid 15 minutes, with the "what to do if it expires" next step (D7). The link is the only secret-shaped thing in the message, and it is hashed at rest and consumed on first POST. With `WEB_URL` unset, the legacy fallback DMs a plaintext dashboard password (see `docs/SECURITY.md`), so the no-credential rule does not apply on that path.
2. **The claim page reuses the claim-form idiom verbatim.** Route `/setup/claim` (GET renders, POST consumes — the exact routes ratified in `ARCHITECTURE-V1.4.md` §4; see §10.1 for the `/setup-password` naming flag). Password + confirm fields with the existing `.secret-field`/`SECRET_TOGGLE` markup, the 6-character floor, and a **static** strength hint — no JS meter, no new dependency.
3. **One generic failure page, no oracle.** Expired, reused, and unknown tokens produce the same page and the same message; the next step is always "send /dashboard in the bot for a new link." The POST shares the login throttle (5 attempts / 900 s) and reuses its exact copy.
4. **Success auto-logs in.** Password set → session created → 303 to `/dashboard?welcome=1`, which flashes one success banner. Other sessions of the account are signed out, mirroring `settings_password`.
5. **MCP tokens stay bot-first (D6).** `/mcptoken create|list|revoke` copy is specified here (§7) — raw token shown once with a store-it-now warning (the `/keys` shown-once pattern), prefix-masked list, cap-reached message, idempotent revoke. **No console surface for token lifecycle in v1.4** (architecture non-goal); one optional cross-link line on `/keys`.
6. **No new tokens, no new components.** Everything here is `door`/`panel`/`secret-field`/`alert`/`copyable`/`btn` — see §11.

---

## 1. Area C — the bot DM (what replaces the password)

### 1.1 Routes (verbatim from `ARCHITECTURE-V1.4.md` §4)

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/setup/claim?token=…` | Validate hash + purpose + expiry (no consume); render the password form | public |
| POST | `/setup/claim` | Shared login throttle; consume token; set password; create session; redirect | public |

Who may generate links: only the two bot commands, for the requesting chat's own account (`seller_id` bound from `msg.from.id`). Admin-minted links are a non-goal.

### 1.2 `/becomeseller` (new web account provisioned)

Exact reply:

```
You are now a seller. Use /setup @channel to link your channel.

Set up your web dashboard with this one-time link (valid 15 minutes):
{setup_url}

Dashboard username: {username}
The link lets you choose a password and signs you in. It works once — if it expires, send /dashboard for a new one. Never share it.
```

- `{setup_url}` = `Config::web_url()` base + `/setup/claim?token={raw}`. `{username}` is the derived account name (Telegram username or `seller_{user_id}`).
- **No password appears anywhere in the message** (verification greps the message constructors — architecture test invariant 4). The placeholder `gen_token(12)` password is still written to the account row but never displayed.
- Failure path unchanged: `Seller created, but web account failed: {e}` (no link minted if the account write failed).

### 1.3 `/becomeseller` (account already exists)

```
You are now a seller. Use /setup @channel to link your channel. Dashboard account already exists; send /dashboard for a sign-in link.
```

### 1.4 `/dashboard` (account exists — sign-in/reset link)

```
One-time dashboard link (valid 15 minutes):
{setup_url}

Dashboard username: {username}
Opening it lets you set a new password and signs you in. It works once — if it expires, send /dashboard again.
```

### 1.5 `/dashboard` (no account yet — provision + link)

```
Web dashboard created.
Dashboard username: {username}
One-time setup link (valid 15 minutes):
{setup_url}

The link lets you choose a password and signs you in. It works once — if it expires, send /dashboard for a new one. Never share it.
```

### 1.6 Copy rules (all four messages)

- Exactly **one link**, exactly **one** TTL statement ("15 minutes"), exactly **one** next-step sentence for expiry. No second link, no QR, no fallback password — a message with two secrets is two secrets to leak.
- "It works once" is stated in every message that carries a link — the single-use property is a feature the seller must understand before they click.
- "Never share it." appears on setup links (new accounts), not on self-service sign-in links (§1.4) — the requester already owns that chat either way; the warning belongs where the account is being born.
- The URL is sent as plain text on its own line (Telegram auto-links it). No Markdown force, no `<`, no HTML.

---

## 2. GET `/setup/claim` — the claim page

### 2.1 Layout

Public page shell: `html_page_bare` + the `.door` wrapper with `.panel.narrow` (the sign-in/claim visual language, `app.css:164-178`). Top-to-bottom:

1. **`h1 Set up your dashboard`** + `<p class="help">This one-time link came from your Telegram bot. It expires 15 minutes after it was sent and works once.</p>`
2. **Alert slot** (empty on first render; §4 error states).
3. **The form** (§2.2).
4. **Footer help**: "Not expecting this? Ignore it — the link stops working after 15 minutes or one use, whichever comes first."

No rail, no nav — this is a door page, visually identical in framing to Sign in / Claim the console.

### 2.2 Form fields

Markup follows `claim_form_html` (`web.rs:651-667`) exactly — `<label for>` + separate `.secret-field` span, because the toggle button must sit inside the field wrapper:

| # | Label | id / name | Type | Validation | Help |
|---|---|---|---|---|---|
| 1 | New password | `setup-claim-password` / `password` | password, `required`, `minlength="6"`, `autocomplete="new-password"` | server-side ≥ 6 chars (`chars().count()`, the `settings_password:1959` rule) | (below the field, shared) |
| 2 | Confirm password | `setup-claim-confirm` / `confirm_password` | password, `required`, `minlength="6"`, `autocomplete="new-password"` | server-side equality with field 1 | "At least 6 characters, typed twice." |
| — | hidden | `token` | hidden, `esc()`'d | — | carries the claim token across failed POSTs (the `claim_form_html` hidden-code pattern, `web.rs:643-650`) — a mistyped confirm must not force a new link |

Submit button: **"Set password and sign in"** — the label promises the auto-login so the redirect is never a surprise.

### 2.3 Strength hint (static, no meter)

Under the confirm field's help, one more line:

`Use a passphrase you don't use anywhere else — 12+ characters is recommended. This console can hold bot, payment and AI credentials.`

- **No JS strength meter.** It would be the page's only script, it fails silently without JS, and the codebase's public pages are script-free. A static, concrete hint ("12+ characters") outperforms a red/green meter at this account's risk level and is fully screen-reader/contrast-safe by construction.
- No banned-password check exists anywhere in the codebase today (verified: `settings_password`, `/setwebpassword`, `claim` — the only rule is the 6-char floor). This spec does not invent one; see §6.6.

### 2.4 Token handling rules

- The raw token is never rendered as text, never logged, never echoed in an error. It appears only in (a) the URL the user arrived with and (b) the hidden form field, both unavoidable and both covered by single-use + TTL.
- GET validates hash + purpose (`'setup_link'`) + expiry **without consuming** — a user who opens the link twice (double-click, preview fetcher) still gets a working form.
- The token in the URL is a bearer credential: it is 48 chars of `gen_token`, SHA-256-hashed at rest, TTL 15 min, single-use, and the POST sits behind the login throttle. That is the D7 recipe; nothing weaker ships.

---

## 3. POST `/setup/claim` — consume, set, sign in

### 3.1 Order of checks (all-or-nothing)

1. **Throttle** (shared login throttle, `login_throttle`, 5 / 900 s). On trip, render the form with `alert-error`: `Too many attempts. Try again in a few minutes.` — the exact string and role/attrs of the login page's throttle banner (`web.rs:715`). The hidden token is preserved so the retry after the window needs no new link. **No token consumption on a throttled request.**
2. **Field checks** (no consume): password `chars().count() >= 6` → else re-render form with `Passwords must be at least 6 characters.`; mismatch → `Passwords do not match.` Both `alert-error`, form values cleared (password fields are never re-populated), hidden token preserved.
3. **Consume**: `consume_pending_token(sha256(token))` → `None` ⇒ the generic invalid page (§4.1) — one message for expired, reused, and unknown, no oracle. **This check runs after the cheap field checks** so a fat-fingered confirm doesn't burn a live token.
4. **Set password** on the returned `account_id` (bcrypt, the existing hash+salt scheme; see §10.2 on the method name), **invalidate the account's other sessions**, **create a new session** for that account, **redirect 303 → `/dashboard?welcome=1`**.

### 3.2 Success state on `/dashboard`

`/dashboard` gains one flash key: `?welcome=1` renders an `alert-success` with `role="status" aria-live="polite"`: `Dashboard password set. You're signed in.` — same PRG-flash vocabulary as `orders_page` (`?saved=1`) and `subs_page`. Re-showing the banner on refresh is accepted (same cost as `?saved=1`); it carries no secret and no state.

No other dashboard changes: the existing connect/restart banners keep their slots and order.

---

## 4. Error & edge states

### 4.1 The generic invalid page (expired / reused / unknown — one page)

Same `.door` + `.panel.narrow` shell, no form (there is nothing to submit):

- `h1 This link has expired` (h2-level reassurance below, not a second heading):
- `alert-error role="alert"`: `This link is invalid or has expired.`
- Help: `Links work once and stop after 15 minutes. Open your Telegram bot and send /dashboard to get a new one.`

- **No oracle:** identical rendering and status (200) for expired, consumed, malformed, and wrong-purpose tokens (the architecture's purpose-separation test asserts an MFA `verify` token fails here identically).
- The message names the fix (bot command), not the cause — the user cannot act on "expired vs reused" anyway.

### 4.2 Other edges

| Case | Behavior |
|---|---|
| Token missing on GET (`/setup/claim` bare) | Generic invalid page (§4.1). Not the form. |
| GET with valid token, account deleted meanwhile (edge) | POST consume still succeeds → password set fails (`account not found`) → `alert-error` `This link is invalid or has expired.` (same page as §4.1; no account-existence leak) |
| Second live link re-minted before use | Both work until one is used or TTL passes — accepted by the architecture (requester owns the chat); no UI change, the older link just dies at first use |
| Setup wizard (`/setup`) not completed | The claim page is independent of `setup_done` — it only needs the pending token and the account row; no redirect to `/setup` |
| JS disabled | Fully functional: no JS on this page at all (the secret toggle is the only enhancement and degrades to always-masked inputs) |

---

## 5. Accessibility (WCAG 2.1 AA — release floor)

1. **Labels.** Both password fields use explicit `<label for>` (the `claim_form_html` pattern) — not `aria-label`-only; `required` + `aria-required="true"`.
2. **Secret toggle.** `SECRET_TOGGLE` ships `aria-label="Show password"` + `aria-pressed` state and the JS binds per-field; nothing new to build.
3. **Errors announced.** Every `alert-error` carries `role="alert"` (assertive announcement); the success flash on `/dashboard` carries `role="status"`. On a failed re-render, focus lands on the alert (web: `tabindex="-1"` on the banner, focused on load).
4. **Help text associated.** The shared help line and the strength hint get ids referenced by `aria-describedby` from the confirm field (and the strength hint from the password field).
5. **Focus visibility.** Global `:focus-visible` ring; the toggle keeps its shipped styles. Tab order: password → toggle → confirm → toggle → submit. No positive `tabindex`.
6. **Contrast & color.** Existing tokens only (verified pairs in `app.css` header); the strength hint is `.help` (ink-3 on surface, the shipped pair); no color-only meaning anywhere.
7. **Touch targets.** Submit button ≥ 44px height at ≤480px (the shipped button padding plus the global rule; verify, don't restyle).
8. **Reduced motion.** No animation on this page; nothing to guard.
9. **Language of the generic page.** One `h1`, one `role="alert"`, real DOM text — announced correctly by every screen reader without live-region tricks.

---

## 6. Security & privacy

1. **No password in any DM** — the architecture's verification invariant; security reviews the message constructors (§1).
2. **Single-use is atomic.** `consume_pending_token` is a DELETE…RETURNING claim — exactly one concurrent caller wins; the loser gets the generic page. Replay fails closed.
3. **Throttle is login-class.** The POST shares the sign-in throttle; token guessing is password-guessing. Key choice (token vs IP) is a web/server call — see §10.4.
4. **Other sessions invalidated on success** (mirror `settings_password`), so a stale session of a possibly-compromised account does not survive a password reset. The new session survives, obviously.
5. **No account-existence oracle.** All failure modes render the §4.1 page; the form never confirms a username or account id.
6. **No banned-password list in v1.4** (none exists in the codebase; the floor is 6 chars server-side). Out of scope here; if security requires one, it belongs in a shared util used by `settings_password`, `claim`, and this page — one rule, three surfaces, not three rules.
7. **TOTP bypass flag (for security).** A successful claim creates a session without a TOTP challenge, even for a TOTP-enabled account — the architecture's "success ⇒ create session" reading. The bot-chat link is the recovery channel by design; security should confirm this is the intended posture. **Flagged, not fixed here.**
8. **URL-borne credential.** The token rides in the query string (that is the design); servers must not log it (no `tracing` of raw query strings on these routes) and `WEB_URL` must be HTTPS in production docs. Security verifies.
9. **CSRF.** Public POST with a bearer-equivalent hidden field; SameSite=Strict cookie posture unchanged. No new token scheme.

---

## 7. Area B — MCP token self-service (bot copy)

Bot-only in v1.4 (D6 non-goals: no console surface, no MCP tool for lifecycle, no admin-mints-for-seller). Admins/owners manage **only their own** tokens. Raw tokens are `gen_token(48)`, stored as SHA-256 hash + 8-char prefix; the cap of 10 active tokens lives in the Db.

### 7.1 `/mcptoken` (no args) — usage + explanation

```
MCP tokens let external tools call this shop's MCP endpoint as you — they see only your own data.

/mcptoken create <name> [days] — mint a token
/mcptoken list — list your tokens
/mcptoken revoke <id> — revoke a token

How to use a token: docs/API.md → MCP section.
```

### 7.2 `/mcptoken create <name> [days]`

- Usage: `Usage: /mcptoken create <name> [days]`
- Success (raw shown exactly once):

```
MCP token created — {name} (prefix {prefix})

{raw_token}

Shown once — store it now. It authenticates as you against the MCP endpoint; see docs/API.md → MCP section.
Expires: {expires}
```

  - `{prefix}` = the stored 8-char prefix, rendered `mcp_ab12…`-style; `{expires}` = `YYYY-MM-DD` for a `days`-limited token, `never` when omitted or `0`.
  - The "Shown once — store it now" line is mandatory and adjacent to the token (the `/keys` shown-once banner pattern, `web.rs:6945`).
- Cap reached (Db `Err` at 10 active): `Token limit reached: 10 active tokens. Revoke one with /mcptoken revoke <id>, then create again.` (A cap race has one winner and one loser — the loser gets this message.)
- Name rules: required arg, trimmed; ≤ 64 chars → else `Token name must be 64 characters or fewer.`; `days` must parse ≥ 0 → else `Days must be a whole number, 0 or more.`

### 7.3 `/mcptoken list`

- Empty: `No MCP tokens. Create one with /mcptoken create <name>.`
- List (header + one line per row, newest first):

```
Your MCP tokens (id | prefix | name | created | expires | last used):
{id} | {prefix} | {name} | {created} | {expires} | {last_used}
```

  - **Prefix-masked only** (`mcp_ab12…`) — the raw token is never reconstructed, re-shown, or hinted at (no length, no last-4 beyond the prefix).
  - Revoked rows stay listed with ` (revoked)` appended to the name; `—` for missing values.
  - Dates render `YYYY-MM-DD` (bot-width readability); `never` for no expiry.

### 7.4 `/mcptoken revoke <id>`

- Usage: `Usage: /mcptoken revoke <id>`
- Success (idempotent — revoking an already-revoked own token repeats it): `Token {id} revoked. Clients using it get errors immediately.`
- Not found / not yours (one message, no oracle): `Token not found (or it does not belong to you).`
- **No confirm step.** The explicit id argument is the confirmation; the success copy states the consequence. Bot-side two-step confirms add friction without adding safety here (the actor already typed the target id).

---

## 8. Where tokens are explained for sellers

### 8.1 Bot

The `/mcptoken` no-args reply (§7.1) is the canonical explainer, and `/help`'s token line (docs, `docs/COMMANDS.md`) mirrors it in one line: `/mcptoken — manage MCP API tokens (create / list / revoke).`

### 8.2 Console `/keys` (optional, one line)

Below the existing help line ("Keys for the REST API — see docs/API.md."), add: `MCP tokens for the booking/catalog MCP tools are managed in the bot: /mcptoken.` — a pointer, not a surface. If the web layer prefers zero changes outside the contracted routes, omitting this line is acceptable; it is a nice-to-have, flagged as such.

### 8.3 docs/API.md gap (must be closed for §7 to ship)

`docs/API.md` has **no MCP section today** (the only MCP mention in `docs/` is `SECURITY.md`'s "MCP tokens (v2, planned)", now stale — D6 wires them in v1.4). The §7 copy points at "docs/API.md → MCP section", so that section must exist before the copy ships: endpoint path, auth header format (`Authorization: Bearer mcp_…`), per-seller scoping, the 10-token cap, and the bot lifecycle commands. **Owner: server writes the endpoint contract; docs lands the doc.** Flagged, not fixed here.

---

## 9. Responsive behavior

The claim page is a single narrow form: `.door` centers it, `.panel.narrow` (max-width 420px) already fits 320px-wide viewports; fields are full-width, the submit button full-width at ≤480px, both fields ≥44px tall. Nothing to build — listed so web verifies at 375px rather than assumes. Bot copy is plain text and wraps naturally.

---

## 10. Backend contracts web/server/core must satisfy (flags, not fixes)

1. **Route naming.** The working brief called this page `/setup-password?token=…`; the ratified contract in `ARCHITECTURE-V1.4.md` §4 is `/setup/claim?token=…` (GET validate, POST consume). **This spec follows the architecture doc.** Flagged for the owner so the brief and the contract stop diverging — drift here is a verification defect either way.
2. **Password-set method name drift.** The arch contract names `set_web_password_by_id` (async); `update_web_password_by_id` already exists in `src/db.rs:3537` with the same shape (async, bcrypt, by id). core should land the arch-named method or ratify the existing name in the doc — one of the two, before server wires the bot. **Owner: core.**
3. **Session invalidation on claim success.** The arch says "set password, create session" and is silent on other sessions. This spec requires the `settings_password` semantics (other sessions deleted, this one kept) because the flow is functionally a password reset. **Owner: server/core.**
4. **Throttle key choice.** The shared `login_throttle` is string-keyed; for `/setup/claim` the key should be the token hash (per-credential) or the peer IP (per-network) — web/server pick one; both are defensible, IP-keying also throttles token *spray* across many fabricated tokens, which per-token keying does not. **Owner: web + security.**
5. **`/dashboard?welcome=1`** is a new flash param on an existing page — the only change to `/dashboard` itself. **Owner: web.**
6. **MCP token name length** (≤ 64 chars) and `days` parsing live in the handler (server); the Db cap of 10 active tokens and the prefix/hash storage are already in the core contract.

---

## 11. Token-contract flags (design)

**None.** Everything reuses shipped primitives: `door`, `panel` (+ `.narrow`), `secret-field`/`secret-toggle`/`SECRET_TOGGLE`, `help`, `btn`, `alert`/`alert-error`/`alert-success`, `code.copyable` (if web later renders a token in a console context — not required in v1.4), `badge` (not needed here). No new classes, no inline styles, no new color values, no new components.

---

## 12. Acceptance checklist (for web / verification)

Setup links (Area C):

- [ ] `GET /setup/claim?token=…` renders the form for a valid, unexpired, unused `setup_link` token; a second GET still renders (no consume on GET).
- [ ] The form has password + confirm fields with the secret toggle, `autocomplete="new-password"`, `minlength="6"`, explicit labels, and the strength-hint line.
- [ ] POST with `consume_pending_token → None` (expired, reused, unknown, or wrong-purpose token) renders the one generic invalid page — identical markup and status for all four.
- [ ] POST success sets the password, creates a session for the right account, invalidates other sessions, and 303-redirects to `/dashboard?welcome=1`, which flashes "Dashboard password set. You're signed in."
- [ ] Failed POSTs (throttle, short password, mismatch) preserve the hidden token and never consume it; the throttle message matches the login page exactly.
- [ ] Two concurrent POSTs with one token: exactly one success.
- [ ] A 16-minute-old token fails GET validation and POST claim.
- [ ] An MFA `verify` token does not work on `/setup/claim`.
- [ ] Neither the token nor any password appears in logs, error pages, or bot messages (sentinel grep test).
- [ ] Keyboard-operable, visible focus, error alerts announced (`role="alert"`), labels programmatic, usable at 375px.

MCP tokens (Area B):

- [ ] `/mcptoken` (no args) returns the usage + explainer with the docs/API.md MCP pointer.
- [ ] `create` DMs the raw token exactly once with the store-it-now warning, prefix + expiry in the reply; the raw token appears in no later `list` output, log, or reply.
- [ ] The 11th active token returns the cap message; revoking one frees the slot.
- [ ] `list` renders id | prefix | name | created | expires | last used, revoked rows marked, `—` for missing values.
- [ ] `revoke` is idempotent for own tokens; a foreign or unknown id gets the one no-oracle message; another seller's token is never revocable.
- [ ] `/keys` optionally carries the one-line `/mcptoken` pointer and nothing more.

---

## 13. Open questions for design

1. **Token contract: none.** §11 stands — no new tokens or components; the claim page is the existing claim-form vocabulary, and the MCP flow is bot-text only.
2. **Non-token, for the record (no response needed unless overridden):** the strength hint is static text rather than a JS meter (§2.3). If design wants a meter, it must be a no-JS-safe enhancement and would be the first script on a public page — the recommended default is static.

---

*End of spec.*
