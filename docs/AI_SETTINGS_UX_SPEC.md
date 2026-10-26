# AI Settings — UX & Accessibility Spec

**Owner:** L6 (Senior UX Designer) — spec only; L7 implements in `src/web.rs`.
**Status:** v1 spec. WCAG 2.1 AA is a release floor and is non-waivable.
**Audience:** L7 (implementation), L8 (handlers/data), L9 (data layer), L10 (security review), L6-C (token contract).

---

## 0. Summary of recommendations

1. **Dedicated page, not a section.** Route `GET /ai` (owner-only), with `POST /ai/save` and `POST /ai/test`. Rationale in §1.
2. **Two panels on one page:** (A) AI provider credentials (form), (B) Usage & cost dashboard. Progressive disclosure: credentials first, dashboard below, so a first-time seller configures then immediately sees (empty) usage.
3. **Write-only secrets**, reusing the existing badge + blank-keeps-current pattern from `settings_body`.
4. **Test connection** is an async, non-navigating action with a polite live region; it never writes `.env` and never records usage.
5. **Usage data is seller-scoped** via the existing `ai_usage_*` methods; the owner page reads the global bucket (`seller_id = 0`) pending L8 confirmation (see §11).

---

## 1. Page route & navigation

### Routes

| Method | Path | Purpose | Auth |
|---|---|---|---|
| GET | `/ai` | Render page (credentials + usage) | owner-only (403 otherwise, matching `forbidden_page`) |
| POST | `/ai/save` | Validate + write AI env keys | owner-only |
| POST | `/ai/test` | Test connection using submitted or saved values; returns JSON | owner-only |

All paths are subject to the existing base-path prefix (`WebState::url`) and the 30s request timeout. `/ai/test` should complete well within it (provider timeout is 60s at the client, but the test call uses a short 10s ceiling — see §4).

### Navigation placement

Add an **"AI"** entry to the rail nav (`rail_nav`) inside the **Operate** group, between **Channels** and **Plans**:

`Dashboard · Channels · AI · Plans · Products · Posts`

Rationale:
- AI is an operational capability (like Channels), not a deployment setting (Settings footer). Grouping it with Operate matches how sellers think ("turn on AI replies").
- Keeping it out of the Settings footer preserves Settings as the bot-connection/password surface and avoids overloading that page.
- A single entry keeps the nav scannable on mobile (the rail already wraps at ≤900px).

The **Settings** page should gain a one-line cross-link under its help text: *"AI provider and usage live on the AI page."* (and vice versa is not needed).

### Why not a section in `/settings`

- `/settings` already owns bot token, owner ID, default channel, Crypto Pay token, and the dashboard password. Adding six AI fields plus a usage dashboard + test connection would make it the longest page in the console and push the usage table below the fold on mobile, hurting the "time-to-first-sale < 10 min" north star.
- Credentials and usage have different lifecycles: credentials change rarely and apply on restart; usage is read live. Separating them lets the usage panel refresh independently later without re-rendering the secret form.
- Security review is simpler when the secret-handling surface is isolated.

### Unconfigured console

If `cfg.ai_enabled == false`, the page still renders fully (so the owner can configure and test before enabling). A non-blocking `alert-info` at the top: *"AI is currently disabled. Configure a provider and key, then toggle Enable AI features."* No redirect.

---

## 2. Information architecture / layout

Top-level order (single column, `max-width: var(--measure)`):

1. **Page header** — `h1 AI` + `.sub` "provider credentials and usage" + (right-aligned) nothing; the primary action lives inside the form.
2. **Status banner slot** — save/test outcomes (§3, §4).
3. **Panel A — "AI provider"** (`article.panel`):
   - Form with all credential fields (§3).
   - Row of two buttons at the bottom: **Save AI settings** (primary, submit) and **Test connection** (secondary, `type="button"`).
   - Write-only secret hint line.
4. **Panel B — "Usage & cost"** (`article.panel`):
   - Headline cards (2×2 grid) — §5.
   - Daily usage table — §5.
   - Recent errors list — §5.
5. **Footer help** — "Values are written to `.env` with 0600 permissions. AI settings apply on restart; Test connection uses the values as typed."

Panel A and Panel B are siblings (not nested). On desktop they stack; we considered a two-column layout but rejected it because the form and the wide table both need full measure width on tablet.

### Panel A internal order (form fields)

1. Enable AI features (checkbox)
2. Provider (select)
3. Base URL (text, shown for `openai`/`custom`; disabled for `anthropic`)
4. API key (password, write-only)
5. Model (text)
6. Daily token budget, global (number)

Then buttons. This order follows the dependency chain (enable → who → where → secret → what → limits).

---

## 3. Field-by-field spec

All fields use the existing `<label>…<input>` wrapping pattern so the label is programmatically associated. Every field carries `aria-describedby` pointing at its `.help` span (use the help span's `id`). Required fields use the `required` attribute plus `aria-required="true"`.

Blank-keeps-current applies to every field **except** the enable toggle and (on save) `AI_ENABLED`, which is always written (§3.1). Secrets are never rendered.

| # | Label | Name | Type | Default | Validation | Help text | Masked display |
|---|---|---|---|---|---|---|---|
| 1 | Enable AI features | `ai_enabled` | checkbox | `cfg.ai_enabled` | none | "Turns on AI-assisted replies and templates. Off means no AI calls are made, even with a key saved." | n/a (boolean) |
| 2 | Provider | `ai_provider` | select: `openai`, `anthropic`, `custom` | `cfg.ai_provider` (fallback `openai`) | must be one of the three | "OpenAI-compatible (including local servers), Anthropic, or any OpenAI-compatible custom endpoint." | n/a |
| 3 | Base URL | `ai_base_url` | text, `inputmode="url"` | `cfg.ai_base_url` | if provided: parses as `http(s)://` URL, ≤512 chars; required when provider is `openai`/`custom` and enabling | "OpenAI-compatible API base. Leave the default for OpenAI; point at your own server for `custom`." | shown verbatim (not a secret) |
| 4 | API key | `ai_api_key` | password, `autocomplete="new-password"` | blank | if provided: 8–4096 chars, no whitespace; if enabling and no saved key and none provided → error | "Write-only. Leave blank to keep the current key. Stored in `.env` with 0600 permissions and never rendered back." | badge only: `configured` (success) / `not set` (warning) |
| 5 | Model | `ai_model` | text | `cfg.ai_model` | if provided: 1–128 chars, no newline | "Model name as the provider expects it (e.g. `gpt-4o-mini`, `claude-3-5-sonnet-20241022`)." | shown verbatim |
| 6 | Daily token budget (global) | `ai_daily_token_budget` | number, `min="0" step="1" inputmode="numeric"` | `cfg.ai_daily_token_budget` (blank if None) | if provided: integer ≥ 0; blank = unlimited | "Maximum tokens per day across all sellers and buyers. Blank = unlimited. Enforced by the AI service." | shown as number or "unlimited" |

### Masked-display rules (secrets)

- The API key is **never** placed in any `value` attribute, `placeholder`, HTML body, JSON response, log line, or error message. The only representation is the badge (`configured` / `not set`) computed server-side from `cfg.ai_api_key` presence, exactly like `token_badge`/`crypto_badge` in `settings_body`.
- The badge is a `<span class="badge badge-success|badge-warning">` with an `aria-label` of "API key configured" / "API key not set" so color is not the only channel.
- If a key is saved, the input placeholder is "leave blank to keep the current key"; if none is saved, placeholder is "sk-… / your provider key" (generic, no real value).
- `autocomplete="new-password"` (not `current-password`) so password managers do not autofill a stale value into the masked field.

### Enable-toggle semantics on save

`AI_ENABLED` is **always written** on a save, as `AI_ENABLED=1` or `AI_ENABLED=0`, reflecting the checkbox. All other fields follow blank-keeps-current. This is the one exception to blank-keeps-current and must be documented in the footer help so an owner who unchecks and saves understands AI is turned off (not "left as-is").

### Validation before writing (server-side, all-or-nothing)

Order of checks (fail fast, no `.env` write on any failure):
1. Provider ∈ {openai, anthropic, custom}.
2. If provider ∈ {openai, custom}: base URL present (saved or submitted) and parses.
3. If enabling: an API key must be available (submitted or already saved). Otherwise error *"Add an API key before enabling AI."*
4. Model, if submitted, non-empty and ≤128 chars.
5. Budget, if submitted, parses as `i64 ≥ 0`.
6. API key, if submitted, 8–4096 chars and no whitespace.

On any failure, re-render `/ai` with an `alert-error` banner and **no** `.env` write (matching `settings_save`'s all-or-nothing contract). On success, write via `config::write_env_file` and render `alert-success`: *"Saved. AI settings apply on restart."*

---

## 4. Test-connection UX

### Endpoint contract (for L8)

- `POST /ai/test`, owner-only.
- Accepts the same form fields as `/ai/save` (so an owner can test values they have not saved yet). Missing fields fall back to saved config.
- If `ai_api_key` is blank, use `cfg.ai_api_key`; if still none → return `{ok:false, latency_ms:0, error:"No API key configured."}` without a network call.
- Build the provider exactly as `main.rs` does (`OpenAiProvider::new(base_url, key)` for openai/custom; `AnthropicProvider::new(key)` for anthropic; anthropic ignores base_url).
- Run one `generate` call with a minimal request: messages `[{role:"user", content:"ping"}]`, `max_tokens: 5`, `temperature: 0.0`, `model` = resolved model.
- Measure wall-clock latency around the call (milliseconds, integer).
- **Do not** record usage (no `record_ai_usage`) and **do not** write `.env`.
- Timeout the test call at 10s (do not wait the full 60s client timeout).
- Response: HTTP 200 with JSON `{ "ok": bool, "latency_ms": number, "error": string|null }`. HTTP 401/403 for auth failures (not for provider failures — those are `ok:false`).
- Error sanitization: strip the API key, the full URL query string, and any `Authorization`/`x-api-key` header text from the provider error. Map to a short category where possible: *"Authentication failed (check the API key)."*, *"Model not found."*, *"Network error."*, *"Timed out after 10s."*, else the provider's message with the key redacted. Never echo the submitted key or base URL credentials.

### UI states

The Test connection button is a secondary button (`type="button"`) placed beside Save. It does not submit the form.

| State | Button | Live region | Visual |
|---|---|---|---|
| Idle | enabled, label "Test connection" | empty | — |
| Loading | disabled, label "Testing…" + spinner (CSS only) | "Testing connection…" | button `aria-busy="true"` |
| Success | enabled, label "Test connection" | "Connected in {n} ms." | green text + check glyph (SVG, not emoji) next to button |
| Error | enabled, label "Test connection" | "Connection failed: {msg}" | red text, message truncated to 2 lines with "Show more" expand (native `<details>`) |

- Under `prefers-reduced-motion: reduce`, the spinner is replaced by the static text "Testing…" (the global CSS rule already disables animation; ensure the spinner is not the only indicator).
- The result text is rendered in a persistent element below the button row (not a toast) so it survives reflow and is readable on mobile.
- Success/error glyphs are inline SVG with `aria-hidden="true"`; meaning is conveyed by text + color (color is not the only channel).

### Live region (a11y)

- Container: `<div id="ai-test-status" role="status" aria-live="polite" aria-atomic="true" class="help">` holding the result text.
- On error, additionally set `role="alert"` on the same element for that update so screen readers announce assertively. (L7: toggle the attribute alongside the text.)
- The region is always present in the DOM (empty when idle) so the first announcement is not missed.
- Do not use `aria-live="assertive"` for loading/success.

### Keyboard & interaction

- `Enter` on the button activates it (native). `Space` too.
- While loading, the button is `disabled` (not `aria-disabled`) so it is removed from the tab order; focus stays put and returns to enabled on completion.
- The button must not trigger form submission (`type="button"`).
- Concurrent clicks are ignored (button disabled during in-flight request).
- JS is a small inline `fetch` (no framework); must degrade gracefully: if JS is disabled, the button does nothing and a `<noscript>` note reads "JavaScript is required to test the connection."

---

## 5. Usage dashboard layout

### Headline cards (Panel B, top)

Four cards in a 2×2 grid (`.grid-2`), each a `<article class="panel">` or a dedicated `.stat` card (see §12):

| Card | Value | Sub-label | Source |
|---|---|---|---|
| Today's tokens | `{n}` (mono, large) | "tokens used today" | `ai_usage_today_tokens(seller)` |
| 7-day tokens | sum of daily tokens | "tokens, last 7 days" | `ai_usage_summary(seller, 7)` |
| 7-day requests | sum of daily counts | "AI requests, last 7 days" | `ai_usage_summary(seller, 7)` |
| 7-day estimated cost | `${x.xx}` (2 decimals) | "estimated cost, last 7 days" | new: `ai_usage_cost_total(seller, 7)` (§11) |

- Numbers use `.mono` (Plex Mono) and a larger size (L6-C: see `.stat-value` token in §12).
- Cost is shown with a tooltip/help: *"Estimated from token counts and rough per-model pricing; not a bill."*
- If AI is disabled, cards still render with whatever historical data exists (do not hide).

### Daily table

Columns (in this order): **Date** · **Tokens** · **Requests**.

- Date formatted `YYYY-MM-DD` (matches stored `substr(created_at,1,10)`); consider a friendlier `Mon 26 Oct`? Keep ISO for sortability and consistency with DB; L7 may format but must remain unambiguous. Recommend ISO + `title` attribute with long form.
- Tokens right-aligned, mono.
- Requests right-aligned, mono.
- Empty state: `<p class="empty">No AI usage yet. Usage appears here after the first AI-assisted reply.</p>` in place of the table.
- Table wrapped in `.table-wrap` for horizontal scroll.
- `<caption class="sr-only">Daily AI usage for the last 7 days</caption>`.

### Recent errors list

Below the table, a sub-panel `<article class="panel"><h3>Recent errors</h3>…</article>`:

- Each row: timestamp (mono, small) · provider/model (mono) · error message (ink-2, truncated to 2 lines; full text in `title` and expandable via `<details>`).
- Max 10 rows, newest first.
- Empty state: *"No recent errors. AI calls are succeeding."*
- Error messages are sanitized server-side (no key, no full URL); if a raw message somehow contains a secret pattern, L8 must redact before rendering. L10 reviews.

### Refresh

v1 is server-rendered on page load only (no auto-refresh, no polling) to keep it simple and accessible. A manual "Refresh" link (GET `/ai`) is acceptable but not required; the browser back/refresh covers it. Note in footer: "Refresh the page to update usage."

---

## 6. A11y requirements (WCAG 2.1 AA)

### Semantics & landmarks
- Page uses the existing shell: `<aside class="rail">` (navigation landmark) + `<main class="main">`. Ensure `<main>` has `id="main"` so a skip link can target it (add skip link if missing — L7).
- One `h1` per page ("AI"), then `h2`/`h3` for panels and subsections. No skipped heading levels.
- Panels are `<article>` or `<section aria-labelledby="…">` with a heading. Prefer `<section aria-labelledby>` for the two main panels so screen readers can navigate by landmark.

### Labels & descriptions
- Every input/select/checkbox has a wrapping `<label>` (existing pattern) — do not switch to `aria-label`-only.
- Help text is associated via `aria-describedby="{field}-help"` and the span has that `id`.
- Badge-only status (configured/not set) gets `aria-label` (§3).
- Required fields: `required` + `aria-required="true"`.

### Focus management
- Tab order follows DOM order: header → Panel A fields (enabled, provider, base URL, API key, model, budget) → Save → Test connection → Panel B cards (static, skipped) → table → errors. No positive `tabindex`.
- Focus ring: rely on global `:focus-visible { outline: 2px solid var(--brand); outline-offset: 2px; }`. Do not override `outline: none` on inputs without providing an equivalent (the existing input focus style uses box-shadow; keep both — outline + shadow — for AA).
- After a save that re-renders the page, focus should land on the banner (L7: put `tabindex="-1"` on the banner container and focus it on load when a banner is present, or use `autofocus` on the first field). Recommend: focus the alert banner when present, else the first field.
- Test connection does not move focus (result announced via live region).

### Contrast
- Use only existing tokens. Verified pairs (from `app.css` header): ink/surface 12.9:1, ink-2/surface 5.6:1, brand-ink/brand 5.1:1; dark mode ink/surface 11.2:1, ink-2/surface 6.1:1. All ≥ 4.5:1.
- Badge text on tinted backgrounds: verify `--brand` on `--brand-tint` and `--warn` on `--warn-tint` ≥ 4.5:1 (they are used elsewhere; reuse, do not invent).
- Error text `--ink` on `--danger-tint` ≥ 4.5:1 (existing `.alert-error`).
- Do not introduce new color values.

### Keyboard
- All actions are native `<button>`/`<a>`/`<input>`. No `div` with click handlers.
- Test connection button `type="button"`.
- Expandable error details use native `<details>`/`<summary>` (keyboard accessible, no JS).

### Reduced motion
- Global rule in `app.css` already disables transitions/animations under `prefers-reduced-motion: reduce`. Do not add new animations without the same guard.
- Test connection spinner: CSS animation only; under reduced motion, hide the spinner and show static "Testing…" text.
- No auto-scrolling, no carousels.

### Screen reader
- Live region as specified in §4.
- Table caption (sr-only).
- Empty states are real text in the DOM (not CSS `::before`).
- Form validation errors are announced via the alert banner's role (use `role="alert"` on `.alert-error`).

### Touch targets
- Buttons and inputs min height 44px on mobile (≥ 375px). Existing padding yields ~36px; L7 should ensure `min-height: 44px` for buttons at ≤480px (or globally) to meet the (advisory) 2.5.5 target. At minimum, never below 24×24 (AA 2.5.5).

---

## 7. Security/privacy UX

- **Write-only secrets.** API key never rendered (§3). Same contract as bot token / Crypto Pay token.
- **Never in errors.** Test-connection and save errors must not contain the key, base URL credentials, or provider error bodies that echo them. L8 sanitizes; L10 verifies with a test that submits a sentinel key and asserts it is absent from the response body.
- **Masked badge.** Only `configured`/`not set` is shown; no partial key, no last-4, no length.
- **No key in URLs.** Test connection uses POST body, never query string. GET `/ai` never accepts or echoes a key.
- **No key in logs.** `tracing` calls must not log the API key or the full base URL (host is fine; query/credentials are not). L10 audits.
- **Owner-only.** Non-owners get the existing `forbidden_page` (403, no partial content), identical to `/settings`.
- **CSRF.** Reuse the existing cookie (`SameSite=Strict`, `HttpOnly`) posture; no new token scheme needed for v1. POST forms are same-origin. L10 may require a CSRF token later (out of scope v1 unless L10 blocks).
- **`.env` permissions.** `write_env_file` already sets 0600 on unix; AI keys inherit this. No change.
- **Restart honesty.** Footer and success banner state that AI settings apply on restart, so an owner is never misled into thinking a saved key is live immediately. Test connection is the only "live" action.

---

## 8. Responsive behavior

Breakpoints (existing in `app.css`): `≤1100px` stacks `.grid-2`; `≤900px` stacks the shell rail and `.form-row`; `≤480px` not explicitly defined but main padding already reduces at 900.

| Viewport | Behavior |
|---|---|
| ≥1100px (desktop) | Headline cards 2×2; form single column within measure; table full width. |
| 900–1100px (tablet) | Headline cards stack to 1 column; form-row stacks; rail still vertical. |
| ≤900px (mobile) | Shell rail becomes horizontal wrapped bar; main padding 20px 16px; cards stack; form fields full width; table scrolls horizontally inside `.table-wrap`. |
| ≤480px (small phone) | Buttons (Save, Test connection) become full-width stacked (`width: 100%`); touch targets ≥44px; help text wraps; badge wraps below its label. |

- No horizontal scroll on the page itself; only inside `.table-wrap`.
- The rail's "AI" entry must remain reachable without horizontal scroll when the bar wraps (it does today).
- Test connection result text must not be clipped; allow wrap.

---

## 9. Empty / error states

### Empty states
- No usage (all cards zero + no table rows): keep cards showing `0` / `$0.00` and replace the table with `<p class="empty">No AI usage yet. Usage appears here after the first AI-assisted reply.</p>`.
- No recent errors: `<p class="empty">No recent errors. AI calls are succeeding.</p>`.

### Error states
- Save failure: `alert-error` banner above Panel A with the reason; form values for non-secret fields are re-populated (secrets remain blank with badge).
- Test connection failure: inline result text (red) + live region announcement; no page reload.
- Data load failure (DB error reading usage): render Panel B with an `alert-error` *"Could not load usage data. Try refreshing."* and omit the table/cards (or show cards as `—`). Do not crash the page.
- Unconfigured AI: `alert-info` (§1), non-blocking.

### Loading states
- Test connection: button disabled + "Testing…" (§4).
- Page load: server-rendered; no client loading skeleton needed. If L7 later adds async usage fetch, use skeleton bars (existing `.empty`-adjacent pattern) — not required for v1.

---

## 10. Out of scope for v1 (explicit)

- Per-model usage breakdown or charts.
- Per-seller budgets / rate limits (budget is global only).
- Cost alerts, budget notifications, or hard-stop banners beyond the service's existing budget check.
- 2FA / re-authentication before saving secrets.
- Hot-reload of AI config without restart.
- Editing or deleting individual usage records.
- CSV export of usage.
- Provider-specific advanced parameters (temperature, max tokens, system prompt) — owned by templates/service, not this page.
- Multi-key rotation / key versioning.
- A seller-facing (non-owner) AI settings page.
- Automated tests for the test-connection JS (L7 may add; not specified here).
- New icon set additions beyond existing inline SVGs.

---

## 11. Data & backend contracts L7/L8/L9 must satisfy

These are not UX but are required for the spec to be implementable; flagged here so the owning rungs can act.

1. **Env-key ownership.** `SETUP_ENV_KEYS` in `src/config.rs` must include `AI_ENABLED`, `AI_PROVIDER`, `AI_BASE_URL`, `AI_API_KEY`, `AI_MODEL`, `AI_DAILY_TOKEN_BUDGET` (or a parallel `AI_ENV_KEYS` set consumed by `write_env_file`). Otherwise `write_env_file` treats existing AI lines as foreign and preserves stale values → duplicates. **Owner: L9 (config) + L8.**
2. **Cost aggregation.** `ai_usage_summary` returns `(date, tokens, count)` but no cost. Add either `ai_usage_cost_total(seller_id, days) -> f64` and (for the table, if cost column is ever added) `ai_usage_cost_summary`. **Owner: L9.**
3. **Recent errors.** Add `ai_usage_recent_errors(seller_id, limit) -> Vec<(created_at, provider, model, Option<error_message>, status)>`. **Owner: L9.**
4. **Seller scoping for the owner page.** Confirm which `seller_id` represents platform/global AI usage (likely `0`). If usage is recorded under multiple seller IDs and the owner should see an aggregate, add `ai_usage_today_tokens_global()` / `ai_usage_summary_global(days)`. **Owner: L8/L9. Decision needed before implementation.**
5. **Test-connection provider build.** Reuse the factory logic in `main.rs`; consider extracting a `build_provider(provider, base_url, api_key) -> Arc<dyn AiProvider>` helper to avoid drift. **Owner: L8.**
6. **Test-connection timeout.** The provider client timeout is 60s; the test path must use a 10s ceiling (e.g., `tokio::time::timeout`) and return a clean error. **Owner: L8.**
7. **Error redaction.** L8 implements a redactor that removes the API key and credential-bearing URL components from provider errors before they reach the JSON/HTML. L10 reviews the redactor. **Owner: L8 + L10.**

---

## 12. Token-contract implications (for L6-C)

The materialized token contract lives in **`assets/app.css`** (served from the binary, no build step). Two implications:

1. **`DESIGN.md` reconciled.** `DESIGN.md` now documents the ledger-green theme
   (`--brand: #1E6B52`, paper `#F4F6F3`, Plex Sans/Mono) and the shipped dark
   theme, matching `assets/app.css`. The AI page uses only `assets/app.css` tokens
   and existing classes — no new color values.
2. **`.stat` / metric card — DECISION: accepted (L6-C).** Add a dedicated `.stat`
   primitive to `assets/app.css` and document it in `DESIGN.md`: `.stat-value`
   (24px, Plex Mono, tabular figures, weight 600) + `.stat-label` (12px, ink-2).
   Container reuses `.panel`. Headline numbers in the AI usage cards, the
   Subscriptions count cards, and the Dashboard all use this primitive instead of
   ad-hoc markup; no inline styles, no new color tokens. Composing from `.panel` +
   `h3`/`p` is no longer the recommended path. (Implementation of the CSS rule is
   tracked with the AI page work; the contract is settled here.)
3. **Rail nav entry.** Adding "AI" to `rail_nav` is a markup change in `web.rs`, not a token change. No CSS needed (entries already styled).
4. **Toggle styling.** Recommend a native `<input type="checkbox">` rather than a custom switch, to avoid a new interactive component and its a11y burden. If L6-C wants a switch visual, it must be a CSS-only skin over the checkbox (no JS, `aria-checked` from native state) and respect reduced motion.
5. **No dark-mode-specific work.** The existing `[data-theme="dark"]` block already covers all tokens; the AI page inherits. L6-C need not add anything.

---

## 13. Acceptance checklist (for L7 / L3 verification)

- [ ] `GET /ai` renders for owner, 403 for seller/other, redirect for anonymous.
- [ ] API key never appears in HTML/JSON/logs (grep test with sentinel).
- [ ] Blank-keeps-current for all non-toggle fields; `AI_ENABLED` always written.
- [ ] Validation rejects bad provider/URL/key/budget before any `.env` write.
- [ ] Test connection returns `{ok, latency_ms, error}` and never leaks the key.
- [ ] Usage cards + daily table + recent errors render; empty states present.
- [ ] All inputs have associated labels; help text linked via `aria-describedby`.
- [ ] Test connection live region announces loading/success/error.
- [ ] `prefers-reduced-motion` disables spinner animation.
- [ ] No horizontal page scroll at 375/768/1024/1440.
- [ ] Focus visible on all interactive elements.
- [ ] `cargo test` (including existing web tests) still passes; new tests for `/ai` access + blank-save + redaction added.

---

*End of spec.*