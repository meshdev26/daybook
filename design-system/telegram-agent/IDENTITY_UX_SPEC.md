# User Identity — Connect, Unlock & Status — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec only; L7 implements in `src/web.rs`.
**Status:** v1 spec, from the ratified identity decision record (2026-10-27), including the supervisor + L10 amendment (conditional 2FA step). WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L9 (data layer), L10 (security review), L6-C (token contract).
**Related:** `DESIGN.md` + `assets/app.css` (token contract), `design-system/telegram-agent/MASTER.md`, `BOT_SWITCHING_UX_SPEC.md` (rail group + bot-scope-pinned idiom), `docs/SETUP_MCP_UX_SPEC.md` (door shell, `SECRET_TOGGLE`, shown-once idiom), `docs/BOOKINGS_UX_SPEC.md` (routes/flash vocabulary). Sibling specs: `IDENTITY_SENDER_UX_SPEC.md`, `IDENTITY_MONITOR_UX_SPEC.md`, `IDENTITY_INBOX_UX_SPEC.md`.

---

## 0. Summary of decisions

1. **One owner-only rail group, "Identity"** — entries Identity / Monitor / Inbox, placed between Operate and Sell. Hidden for seller/admin with the same gating idiom as every other role-gated group: the entries are simply never rendered, and the routes reject server-side.
2. **`/identity` is the status home.** Connecting is a separate, focused door-shell wizard (`.door` + `.panel.narrow`, no rail): one step per screen, one POST per step, named submit buttons, no auto-advance.
3. **Conditional 2FA step (the ratified amendment).** When Telegram reports the account is protected by a password (`PasswordRequired` / `SESSION_PASSWORD_NEEDED`), the wizard inserts step 5a — a transient 2FA password field that is never stored and is explicitly distinct from the unlock passphrase. Without 2FA the step does not exist.
4. **The unlock passphrase is the AEAD key source and is honestly labeled.** `autocomplete="off"` on every credential field (ratified verbatim; overrides the claim form's `new-password` precedent — see §9).
5. **ToS-risk acceptance appears exactly twice:** wizard step 1, and a separate "Enable posting" panel on `/identity` (each writes its own record; the second one writes the posting gate).
6. **Restart unlock is a banner, never a sign-in.** While the identity is locked, every console page carries an `alert-warn` banner above the measure with an inline passphrase field and an **Unlock** button. The banner is focused on render; success is announced via `role="status"`; identity actions are gated server-side while locked.
7. **Identity status is a pinned fact line in the rail** under the bot selector (the `bot-scope-pinned` idiom): label "Identity" + a status word — never merged with bot scope, never color-only.
8. **No new tokens.** Two small CSS additions are authorized and stated explicitly (§9): `.alert-form` here, `.timeline` in the Monitor spec. Everything else is existing primitives.

---

## 1. Routes & navigation

### 1.1 Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/identity` | Status home: state, phone, posting gate, connect entry point | session required; owner only |
| GET | `/identity/connect` | Connect wizard — renders the **current** step from server-side wizard state | session required; owner only |
| POST | `/identity/connect/accept` | Step 1 — record the risk acceptance | session; owner only |
| POST | `/identity/connect/credentials` | Step 2 — api_id + api_hash | session; owner only |
| POST | `/identity/connect/phone` | Step 3 — phone; asks Telegram to deliver the login code | session; owner only |
| POST | `/identity/connect/code` | Step 4 — verify the login code | session; owner only |
| POST | `/identity/connect/password` | Step 5a — Telegram 2FA password (only when Telegram demanded one) | session; owner only |
| POST | `/identity/connect/passphrase` | Step 5b — set the unlock passphrase; completes the connection | session; owner only |
| POST | `/identity/unlock` | Unlock banner submit — available on every console page while locked | session; owner only |
| POST | `/identity/posting` | Enable posting — the second acceptance; writes the gate + its own audit record | session; owner only |

All paths take the base-path prefix (`WebState::url`). Unauthenticated → 303 `/login`. Non-owner → the existing `forbidden_page` (403) — the rail never renders the group for them, so the UI never offers what the server refuses (BOT_SWITCHING §2 precedent).

### 1.2 Rail group

Rendered in `rail_nav` only when `session.role == "owner"` — the same conditional append the owner-only manage entries (AI, Crypto, Admins…) already use — inserted **after the Operate loop, before the Sell group heading**:

```html
<div class="group">Identity</div>
<a href="{base}/identity">Identity</a>
<a href="{base}/monitor">Monitor</a>
<a href="{base}/inbox">Inbox</a>
```

- Entries follow the existing nav markup exactly (`active` + `aria-current="page"` on the current page). No CSS — entries are already styled.
- The group heading stays a plain `<div class="group">` — non-interactive, out of the tab order (BOT_SWITCHING §4 rule).
- Sellers and admins never see the group, and every `/identity*`, `/monitor`, `/inbox` route re-derives the role from the session server-side. The buttons are affordances, not the gate.

### 1.3 Pinned identity status line (rail, every console page)

Under the bot-scope control, in the `bot-scope-pinned` idiom (label + value, a fact — not a widget). Owner-only; absent for everyone else; absent on door pages. Exactly one of:

| Identity state | Markup |
|---|---|
| Connected, unlocked | `<div class="bot-scope bot-scope-pinned" aria-label="Personal identity"><span class="help">Identity</span><span class="badge badge-success">connected</span></div>` |
| Connected, locked | same shell, `<span class="badge badge-warning">locked</span>` |
| Not set up | same shell, `<a href="{base}/identity">Set up identity</a>` — a quiet link, no button chrome |

Rules:

- **Never merged with bot scope.** It is its own `.bot-scope` block below the selector; the two facts never share a line or a label.
- **Word + state, never color alone.** The badge carries the word ("connected" / "locked"); the leading dot is decorative. A screen reader reads "Identity, locked".
- "locked" links nothing and toggles nothing — the unlock affordance is the page banner (§4), which is already on screen.

---

## 2. Identity status home (`/identity`)

Single column, `max-width: var(--measure)`, standard console shell. Top-to-bottom:

1. **Page head** — `h1 Identity` + `.sub` "your personal Telegram account — the one that posts and replies as you".
2. **Banner slot** — PRG flashes (§2.2).
3. **Status panel** (`<article class="panel">`, `h3 Status`) — a `.kv` list: **Status** (badge word: `connected` / `badge-warning` `locked`) · **Phone** (`.mono`, rendered as stored — it is the owner's own account on an owner-only page) · **Posting** (`badge-success` `enabled` or neutral `off`) · **Connected** (`.mono` date). No secrets: api_hash is never rendered in any form.
4. **"Enable posting" panel** (§5) — present while the identity exists and the posting gate is unset; once accepted it collapses to a one-line record (`badge-success` `posting enabled` + `.help` "Accepted {date}.").
5. **Not connected** — when no identity exists, the page is one panel with `<p class="empty"><b>No personal identity connected.</b> Connect one to post and reply as yourself.</p>` and the page's single primary action: **Connect personal identity** (`<a class="btn" href="{base}/identity/connect">`). The Enable-posting panel cannot exist before an identity does.

### 2.1 What the status home never shows

- No api_hash, no session blob, no "test connection" button that sends a Telegram message.
- No disconnect/reconnect in v1 (§10, follow-ups) — a destructive path that touches the owner's live Telegram authorization deserves its own ratified pass.

### 2.2 Flash keys (PRG)

| Param | Banner | Class / role |
|---|---|---|
| `?connected=1` | `Personal identity connected.` | `alert-success` / `role="status"` |
| `?unlocked=1` | `Identity unlocked.` | `alert-success` / `role="status"` |
| `?posting=1` | `Posting as your personal account is enabled.` | `alert-success` / `role="status"` |
| `?error=locked` | `That action needs an unlocked identity — unlock it in the banner above.` | `alert-error` / `role="alert"` |

---

## 3. Connect wizard (`/identity/connect`)

### 3.1 Shell and state machine

- **Focused door-shell page:** `html_page_bare` + `.door` + `.panel.narrow` (the sign-in/claim visual language). No rail, no nav — this is a console flow wearing the door frame; the session is required (unauthenticated → 303 `/login`, non-owner → 403).
- **One step per screen.** The wizard state lives **server-side, keyed to the session row, in memory only** — a process restart resets the wizard, and nothing secret (api_hash, phone, code, 2FA password) reaches disk before the unlock passphrase exists (§7).
- **One POST per step, no auto-advance.** Each step's form POSTs to its own route (§1.1), the handler validates, advances the state machine, and PRG-redirects back to `GET /identity/connect`, which renders the next step. The URL never changes and never carries a step number or a secret.
- **Back/refresh honesty:** `GET` always renders the *current* step. Browser-back re-renders the current step (never a stale earlier form), so a code step cannot be resubmitted blind. No step skips ahead because a field was filled previously.
- **One form per screen.** Each wizard page renders exactly one `<form>`; there is no cancel link in v1 (navigating away abandons the in-memory state — that *is* cancel).
- **Named submit buttons.** Each button names what the POST does: "Continue" (steps 1–2), "Send code" (step 3 — it literally asks Telegram to deliver), "Verify code" (step 4), "Verify password" (step 5a), "Connect identity" (step 5b).
- **Step counter, honest about the conditional step:** a `.help` line — "Step {n} of {total}". Before step 4 the total is 5; if step 4 reveals 2FA, steps render "of 6" from 5a onward. Step 4 carries one extra `.help` line: "If your account uses two-factor authentication, one more step follows."

### 3.2 Step 1 — risk acceptance

- `h1 Connect your personal identity` + `.help` "Step 1 of 5".
- `alert-info`: "This console will sign in to Telegram as **you** — your personal account, not a bot. Posts, replies and reads are attributed to your account and are visible to everyone on Telegram as yours."
- Plain risk copy (body text, not a `details`), three sentences, concrete:
  "Telegram may limit or suspend accounts that automate actions. Anything this console sends — channel posts, replies, reads — is sent by your account and counted against it. Anyone with access to this console can act as you."
- Checkbox, rendered **unchecked**: `<input type="checkbox" id="tos-accept" name="accept" required aria-required="true">` + `<label for="tos-accept">I understand the risks</label>`. The `required` attribute gives a native no-JS stop; the server still re-checks (§3.7).
- Submit: **Continue**.

### 3.3 Step 2 — api_id + api_hash

| # | Label | id / name | Type | Attributes | Help |
|---|---|---|---|---|---|
| 1 | API ID | `connect-api-id` / `api_id` | text, `required` | `inputmode="numeric"` `autocomplete="off"` | "From my.telegram.org → API development tools." |
| 2 | API hash | `connect-api-hash` / `api_hash` | password, `required` | `autocomplete="off"`; wrapped in `.secret-field` + `SECRET_TOGGLE` | "From my.telegram.org. Stored encrypted with the unlock passphrase you choose in a later step — never shown again after this." |

- Submit: **Continue**. The help line names my.telegram.org, per the ratified record — it is the only legitimate source and the copy says so.
- The api_hash reuses the shipped secret-field idiom (label, then the wrapper as a sibling — the toggle must never nest inside the label).

### 3.4 Step 3 — phone

| # | Label | id / name | Type | Attributes | Help |
|---|---|---|---|---|---|
| 1 | Phone number | `connect-phone` / `phone` | tel, `required` | `inputmode="tel"` `autocomplete="tel"` | "International format, e.g. +49 170 1234567. Telegram delivers the login code to this account's Telegram app." |

- Submit: **Send code** — the button name is the honest description of the POST: it asks Telegram to deliver a login code.
- A `.help` line under the button: "Telegram sends the code to your Telegram app first — not by SMS."

### 3.5 Step 4 — login code

| # | Label | id / name | Type | Attributes | Help |
|---|---|---|---|---|---|
| 1 | Login code | `connect-code` / `code` | text, `required` | `inputmode="numeric"` `pattern="[0-9]*"` `autocomplete="one-time-code"` | "The code Telegram just sent to your app. It is not your 2FA password." |

- Submit: **Verify code**.
- If Telegram reports the code request expired, the wizard **rewinds to step 3** with an `alert-warn` (role="alert"): "The code request expired — send a new one." The phone field is preserved; one form per screen is kept by rewinding rather than adding a second form.

### 3.6 Step 5a — Telegram 2FA password (conditional, the ratified amendment)

- **Rendered only when step 4's verification comes back with Telegram's PasswordRequired** (`SESSION_PASSWORD_NEEDED` class of errors). When the account has no 2FA, this step never exists — the wizard goes straight from the code to the passphrase.
- `alert-info`: "This account is protected with two-factor authentication. Type its password to finish signing in."
- Field: label **Telegram 2FA password** (`connect-2fa` / `password`), type password, `required`, `autocomplete="off"`, wrapped in `.secret-field` + `SECRET_TOGGLE`. Help: "Sent to Telegram to finish this sign-in. Used once and never stored — this is **not** your unlock passphrase."
- Submit: **Verify password**.
- The distinction from the unlock passphrase is stated twice — in the help line here and in step 5b's copy — because the two secrets protect different things and confusing them is the one unrecoverable mistake this flow allows (§3.7, §7).

### 3.7 Step 5b — unlock passphrase

- Copy (body text, honest about consequence): "Choose an unlock passphrase. It protects the stored Telegram session at rest — nothing works after a restart until you type it again. It is not stored anywhere and cannot be reset: if you forget it, the stored identity is unreadable and you must disconnect and reconnect."

| # | Label | id / name | Type | Attributes | Help |
|---|---|---|---|---|---|
| 1 | Unlock passphrase | `connect-passphrase` / `passphrase` | password, `required`, `minlength="8"` | `autocomplete="off"`; `.secret-field` + `SECRET_TOGGLE` | "This becomes the key that encrypts your stored session." |
| 2 | Confirm passphrase | `connect-passphrase-confirm` / `confirm_passphrase` | password, `required` | `autocomplete="off"`; `.secret-field` + `SECRET_TOGGLE` | "Typed twice, like a password." |

- Submit: **Connect identity**. Success → PRG `/identity?connected=1`.
- Mismatch re-renders step 5b with `alert-error` "Passphrases do not match." — the fields are cleared, never re-populated.

### 3.8 Wizard error table

Every failure re-renders the **current** step, moves focus to the banner (banner container carries `tabindex="-1"`, focused on load — SETUP_MCP §5.3 idiom), and preserves the wizard state. Field values are cleared for every secret field.

| Case | Banner copy | Class / role |
|---|---|---|
| Step 1 refusal (unchecked box reaches the server) | "You must accept the risks before connecting. Nothing was sent to Telegram." | `alert-error` / `role="alert"` |
| Invalid phone | "That phone number doesn't look right. Use international format, e.g. +49 170 1234567." | `alert-error` / `role="alert"` |
| Wrong login code — Telegram reports attempts left | "Telegram rejected that code. Attempts remaining: {n}." | `alert-error` / `role="alert"` |
| Wrong login code — no count reported | "Telegram rejected that code. Try again — repeated failures trigger a temporary wait." | `alert-error` / `role="alert"` |
| 2FA password rejected | "Telegram rejected the 2FA password. It was not stored. Try again — repeated failures trigger a temporary wait." | `alert-error` / `role="alert"` |
| FloodWait (any step) | "Telegram is limiting sign-ins right now. Try again after {YYYY-MM-DD HH:MM UTC} (in {n} minutes)." | `alert-error` / `role="alert"` |
| AuthKeyDuplicated | "Telegram refused this session: this api_id / api_hash pair is already authorized elsewhere. **Nothing was connected** — nothing was written." | `alert-error` / `role="alert"` |
| Passphrase mismatch / too short | "Passphrases do not match." / "Use at least 8 characters." | `alert-error` / `role="alert"` |

Honesty rules baked into this table:

- **Attempts remaining** are stated only when Telegram reports a number; the console never invents one. The word is "attempt(s) remaining", not "tries left".
- **FloodWait** states the wall-clock retry time — an absolute UTC stamp plus a relative "in {n} minutes". Exact formatting is a carried follow-up (§10).
- **AuthKeyDuplicated explains nothing was connected** — the failure is Telegram's refusal, and the copy promises the all-or-nothing property the implementation must actually have (§7).

---

## 4. Restart-unlock banner (every console page while locked)

The unlock passphrase is the AEAD key; it is never persisted, so after every app restart the identity comes up **locked** and stays locked until the operator types the passphrase in the console.

### 4.1 Placement and structure

Rendered by `html_page` when: session is owner **and** an identity exists **and** it is locked. It is the first element inside `<main>`, **above the measure** — before the page head and every page's content — on every console page. Never a full-viewport takeover; the rest of the console stays readable beneath it.

```html
<div class="alert alert-warn unlock-banner" tabindex="-1">
  <b>Personal identity locked.</b>
  Posting, replying and monitoring as you are paused until it's unlocked.
  <form method="post" action="{base}/identity/unlock" class="inline-form alert-form">
    <label for="unlock-passphrase" class="sr-only">Unlock passphrase</label>
    <input type="password" id="unlock-passphrase" name="passphrase"
           autocomplete="off" required placeholder="Unlock passphrase">
    <button type="submit" class="btn btn-sm">Unlock</button>
  </form>
</div>
```

- The shipped class is **`alert-warn`** (`app.css:427`). The ratified record wrote "alert-warning"; this spec follows the shipped token contract — one name, one drift note, §9.
- The sr-only label keeps the row inline while satisfying the programmatic-label floor; the placeholder is a hint, never the only label.

### 4.2 Behavior

- **Verb discipline:** the word is *locked* / *unlock* everywhere — banner, button, flashes, badges, audit strings. Never "sign in", never "log in", never "password" alone. This is a session key for a stored secret, not an authentication flow, and the vocabulary must not blur into the console login.
- **Focused on render:** the banner container carries `tabindex="-1"` and receives focus on load while present — the operator's first tab stop is the fix for the state they are looking at. No positive tabindex anywhere.
- **Success:** POST → unlock attempt → PRG back to the Referer's console-local path (the `bot/select` referer discipline) with `?unlocked=1`, which renders `alert-success` / `role="status"` "Identity unlocked." — announced politely. The banner is gone from the returned page.
- **Failure:** the banner re-renders on the returned page (still locked) with a `role="alert"` line inside it: "That passphrase did not unlock it." The mechanism is honest by construction — the passphrase derives a key, the AEAD authentication tag fails or holds; there is no stored hash to compare against (§7). Attempts sit behind the login throttle (5 / 900 s, the `login_throttle` posture); the trip message is the login page's exact throttle string. **L10 confirms the throttle key (per-account) — flagged, §7.**
- **Server-gated while locked:** the banner is an affordance, not the gate. Every identity-consuming action — wizard completion steps, `/identity/posting`, user-posting binding (`IDENTITY_SENDER` §3), `/monitor` reads, `/inbox` sends — is rejected server-side while locked, each with a flash that points at the banner (`?error=locked`). Enable-posting is gated too: a uniform rule is cheaper to verify than a per-route exception list, and the banner is on the same screen.

---

## 5. Enable-posting gate (second and last ToS surface)

On `/identity`, below the status panel, while the identity exists and the gate is unset:

- `article.panel` + `h3 Enable posting`.
- Risk copy (the same three risks as wizard step 1, same wording — one vocabulary, two surfaces): "Posting as your personal account sends channel posts from **you**, not the bot. Telegram may limit or suspend accounts that automate actions, and members see the posts as yours. Anyone with console access can post as you."
- Unchecked checkbox: `<input type="checkbox" id="posting-accept" name="accept" required aria-required="true">` + `<label for="posting-accept">I understand the risks</label>`.
- Submit: **Enable posting** → `POST /identity/posting`.
- Refusal (unchecked box reaches the server): `alert-error` / `role="alert"` "You must accept the risks before posting as your personal account. Nothing changed." Focus to the banner.
- Success: PRG `/identity?posting=1`; the handler writes the posting gate **and its own audit record** (`log_action`, action `identity_posting_enabled`) — the acceptance is a record, not just a flag. The wizard's step-1 acceptance writes its own record (`identity_tos_accepted`) the same way.
- **Exactly twice, and that is the cap.** No third copy of the risk text may appear anywhere in the product; the sender flows point at this gate rather than re-asking (IDENTITY_SENDER §3).

Once accepted, the panel collapses to the record: `badge-success` "posting enabled" + `.help` "Accepted {date}. Removing the gate is a v2 decision — see follow-ups."

---

## 6. WCAG 2.1 AA floor (non-waivable)

1. **Labels.** Every field has a programmatic `<label for>` — including the unlock banner's sr-only label and the two checkboxes (label wraps the control text and references it). Placeholders are never the only label.
2. **Focus.** Error banners and the unlock banner carry `tabindex="-1"` and receive focus when they render (§3.8, §4.2). Global `:focus-visible` ring untouched; no `outline: none`; no positive `tabindex`.
3. **Contrast.** Existing token pairs only — `alert-warn` text is `--ink` on `--warn-tint` (the shipped AA pair), `.help` is `--ink-3` on surface, badges use the shipped tint pairs. No new colors.
4. **Screen-reader semantics.** Errors are `role="alert"` (assertive); successes are `role="status"` + `aria-live="polite"` — the exact `subs_page` vocabulary. The wizard is a sequence of full page loads, so each step is announced as a new document naturally.
5. **No color-only state.** Identity state is a word in a badge ("connected", "locked", "posting enabled"); the dot is decorative. Form errors are text in a banner, never a red field alone.
6. **Keyboard.** One form per wizard screen; the banner form is native controls — tab to the field, type, Enter submits. Nothing depends on JavaScript; `SECRET_TOGGLE` degrades to always-masked fields without it (shipped behavior).
7. **Touch targets.** The banner's `.btn-sm` Unlock button keeps a ≥ 44px hit area at ≤ 480px (grow padding on small screens rather than restyling the class).
8. **Reduced motion.** No animation in this flow; nothing to guard. The wizard never auto-advances, auto-submits, or auto-refreshes.
9. **Honest autocomplete.** `autocomplete="off"` on api_id, api_hash, 2FA password, passphrase fields; `autocomplete="one-time-code"` on the login code and `autocomplete="tel"` on the phone — each attribute matches what the field actually is (§9 records the deliberate `off` deviation from the claim form).

---

## 7. Security & privacy (L10 flags)

1. **The 2FA password is transient.** It exists for exactly one POST, is forwarded to Telegram, and is then dropped — never written to the wizard state, the DB, a log line, or an error render. **Owner: L8; L10 verifies the constructors.**
2. **The unlock passphrase is never stored** — not hashed, not verifiable by comparison. Unlock = derive key from passphrase → attempt AEAD open of the session blob → tag verifies or the attempt failed. Failure copy says exactly that and nothing more.
3. **Wizard state is memory-only.** api_id/api_hash/phone/code live in server-side in-memory state keyed to the session until the passphrase step encrypts them at rest. A process restart resets the wizard — the copy on step 2 says storage begins only after the passphrase exists. **Owner: L8/L9.**
4. **No secrets in URLs or logs.** All wizard steps are POSTs; audit rows reference action names and ids only; FloodWait renders a time, never the request payload. Sentinel grep test (SETUP_MCP §12 idiom) covers every string in §3.
5. **AuthKeyDuplicated is all-or-nothing.** That path must leave zero writes — no partial identity row, no wizard residue. The §3.8 banner copy promises it; L3 verifies it.
6. **Unlock throttle.** Wrong-passphrase attempts share the login throttle; key choice (per-account id) is an L10 call — flagged, not fixed here.
7. **CSRF.** Existing SameSite=Strict cookie posture; no new token scheme.
8. **Restart-unlock is the feature, not a gap.** The key existing only in memory means a stolen disk or a dumped DB cannot produce a working identity session. L10 should review the banner's inline-field posture (§10, follow-ups) with exactly that threat model.

---

## 8. Backend contracts L7/L8/L9 must satisfy (flags, not fixes)

1. **Wizard state store** — in-memory, per-session, with the step enum, the pending Telegram code request, and a `password_required` flag set by step 4. **Owner: L8 (shape), L9 (storage posture).**
2. **Telegram user-account client** (sign-in, 2FA, FloodWait, AuthKeyDuplicated surfacing) — new capability; the bot client does not cover it. Error taxonomy must reach the UI as the §3.8 cases, raw strings never rendered. **Owner: L8.**
3. **`identity_locked()` helper** consulted by `html_page` (banner render) and by every identity-consuming handler (server gate). **Owner: L7/L8.**
4. **Audit records** — `identity_tos_accepted`, `identity_posting_enabled`, `identity_unlock` (success; failures logged as attempts without the passphrase). **Owner: L8.**
5. **Flash plumbing** — the §2.2 and §4 keys on their respective redirect targets. **Owner: L7.**

---

## 9. Token-contract flags (L6-C)

1. **New CSS, authorized, one rule block:** `.alert-form` — the unlock banner's inline field row. `.alert` is a flex row and shipped `input { width: 100% }` breaks inside it; the rule keeps one row (flex, wrap allowed, input `flex: 1 1 220px; width: auto` inside the form, min 44px height). This is the only new CSS in this spec; without it the banner needs a modal, which the ratified record excludes.
2. **Class-name drift, recorded:** the ratified record says "alert-warning"; the shipped class is `alert-warn` (`app.css:427`). Spec follows the shipped token contract. `badge badge-warning` (§1.3, §5) is the real shipped name and is used as written.
3. **Deliberate deviation from the claim-form idiom:** credential fields use `autocomplete="off"` (ratified verbatim), where `/setup/claim` uses `new-password`/`current-password`. Password managers will not offer to save these fields; that is the intended posture for keys that are never stored. L6-C may revisit — the spec ships `off` until then.
4. **Everything else is existing primitives:** `door`, `panel` (+ `.narrow`), `secret-field`/`SECRET_TOGGLE`, `alert`/`-info`/`-warn`/`-error`/`-success`, `badge` (+ variants), `btn`/`btn-sm`, `help`, `kv`, `mono`, `empty`, `sub`, `sr-only`, `bot-scope-pinned`. No new components, no inline styles, no new color values.

---

## 10. Acceptance checklist (for L7 / L3 verification)

- [ ] Rail shows the Identity group (Identity / Monitor / Inbox) between Operate and Sell for the owner only; sellers/admins see neither group nor routes (403).
- [ ] Pinned status line reads Identity + connected / locked / Set up identity, sits under the bot selector, and is absent for non-owners and on door pages.
- [ ] Wizard renders in the door shell (no rail), one step per screen, one form per screen, named submit buttons, no auto-advance; refresh and back always show the current step.
- [ ] Step 1 checkbox is unchecked; a server-side refusal renders the §3.8 banner with focus on it and touches nothing.
- [ ] Step 2 help names my.telegram.org; api_hash is a secret field; every credential field carries its §3 autocomplete attribute.
- [ ] With 2FA on the account, step 5a appears exactly once between code and passphrase; the copy distinguishes it from the unlock passphrase; without 2FA it never renders.
- [ ] Wrong code / 2FA fail / FloodWait / AuthKeyDuplicated each render their §3.8 banner; attempts-remaining appears only when Telegram reports it; FloodWait shows an absolute UTC time.
- [ ] AuthKeyDuplicated leaves zero persisted writes.
- [ ] While locked: every console page shows the banner above the measure, focused on render; verb is unlock throughout; success is `role="status"` "Identity unlocked."; failure is `role="alert"` inside the banner.
- [ ] While locked: `/identity/posting`, binding, `/monitor`, `/inbox` sends are rejected server-side with flashes pointing at the banner.
- [ ] ToS risk copy appears exactly twice (wizard step 1, Enable posting); each acceptance writes its own audit record; Enable posting writes the gate.
- [ ] No secret (api_hash, phone, code, 2FA password, passphrase) appears in any URL, log line, or error render (sentinel grep).
- [ ] Keyboard-operable end to end; visible focus; 44px Unlock target at ≤ 480px; no inline styles beyond the authorized `.alert-form` rule.

---

## 11. Follow-ups (non-blocking)

1. **Unlock-banner posture — pending L10.** Inline passphrase entry on every console page is convenient but puts a secret field on every screen (shoulder-surf, autofill, screen-share exposure). L10 rules between: (a) this banner as specced, (b) a banner that links to a dedicated door-shell unlock page, (c) banner + shorter memory lifetime. The verb, gating, and focus semantics in §4 are stable under all three.
2. **FloodWait formatting.** §3.8 shows `{YYYY-MM-DD HH:MM UTC} (in {n} minutes)`; the absolute stamp is UTC by rule (the console stores UTC), but a localized rendering pass is deferred. Never render Telegram's raw seconds integer.
3. **Disconnect / reconnect flow.** Deliberately out of v1 (§2.1): it is destructive (destroys the stored session), T2-adjacent, and deserves its own ratified copy pass.
4. **Live-region confirmation of unlock.** The `?unlocked=1` banner announces on the redirected page; if the console later grows client-side fetches, a persistent `role="status"` region becomes worth revisiting (BOT_SWITCHING §7.1 precedent).
5. **Posting-gate removal.** Once enabled, the gate has no UI path back (§5). If product wants "Disable posting", it needs its own record and copy — not a silent uncheck.

---

*End of spec.*
