# Setup & Management UX — Polish Spec

**Owner:** design. Status: v1 spec (2026-10-29), polish pass — no new routes, no
schema changes, no security-model changes. The web layer implements in `src/web/**`.
**Audience:** web implementation, server data, security review.
**Problem:** the owner says setup and management are "a bit complicated to set up."
**Goal:** time-to-first-value < 10 min on a phone, for a non-technical business owner.
**Floor:** WCAG 2.1 AA is non-waivable (§5). House tokens: `assets/app.css` + `DESIGN.md`;
`MASTER.md` is non-authoritative. Patterns to reuse live in `src/web/wizard.rs`
(`focused_error_alert`, "Step N of M" `.help` lines, secret-field toggle, restart honesty).

**Operational truth the UI must never lie about:** bot tokens live only in `.env` /
`BOTS_CONFIG` (bots.toml) and are read once at startup; the console can never display or edit a
token; the DB `bots` table (key/owner/default_channel/enabled) is also boot-loaded, so add /
enable / disable / delete all need a restart to take effect. Every surface that writes config must
set `restart_required` (the flag `restart_required_banner` in `src/web/dashboard.rs` reads) —
today several do not (see F3.6).

---

## 1. Findings and prescriptions, per surface

### F1 — First-run setup (`src/web/channel_ops.rs`: `setup_page`, `setup_submit`, `setup_error`, `setup_skip`)

**F1.1 — A single validation mistake destroys the whole form.**
`setup_error` renders an alert plus a "Back to setup" link. Every field — the pasted token, the
owner ID, the chosen password — is lost. A non-technical owner who mistypes the owner ID retypes
everything. The wizard solves this by re-rendering the step in place (`focused_error_alert`,
non-secret values preserved); setup must do the same.

*Prescription.* Extract the setup form into a builder (same file, e.g.
`setup_form_body(state, alert: Option<&str>, preserved: &SetupPreserved)` where
`SetupPreserved` holds `owner_id`, `default_channel`, `web_username` only). On any validation
failure re-render the full setup page in place with a `focused_error_alert` — never a link back.
Secrets (token, crypto token, password) are re-rendered empty, matching the wizard's
never-re-render-a-secret rule; the help copy under the token field already tells the user where
to re-copy it from.

**F1.2 — Errors arrive one per round-trip, and Telegram is called before local checks.**
`setup_submit` validates the token via a network call to `getMe` *before* parsing the owner ID or
checking the password. A user with two typos waits on Telegram, fixes the token, then discovers
the password rule. Copy is also wrong on network failure: "Invalid bot token (Telegram rejected
it)" is shown when the console simply could not reach Telegram.

*Prescription.* In `setup_submit`, do all local validation first and report every problem in one
alert (`<br>`-separated lines are acceptable inside the alert). Swap
`validate_bot_token(token)` for `validate_token_username(token)` (already `pub(super)` in
`src/web/admin_console.rs`, no new route) so Telegram's own reason and the bot username are
available. Exact strings:

- Token rejected: `Telegram rejected that bot token. Compare it against the message @BotFather sent you — it looks like 123456:ABC-DEF…. Nothing was saved.`
- Network failure: `Couldn't reach Telegram to check the token. Check your connection and try again. Nothing was saved.`
- Owner ID: `Owner ID must be the number @userinfobot replies with — for example 123456789. Nothing was saved.`
- Password: `Choose a dashboard password of at least 6 characters. Nothing was saved.`

Every one of these renders via `focused_error_alert` inside the re-rendered form (F1.1).

**F1.3 — No orientation: one dense 7-field form with no step sense.**
The page is titled "Welcome — first-run setup" but gives no sense of how many steps exist or what
happens after Save. Required and optional fields are interleaved (Default channel and Crypto Pay
token sit between the required ones).

*Prescription.* Keep the single POST (no new routes). Restructure visually:

- Under the h1: `<p class="help">Two steps. This page is step 1 — step 2 is restarting the app.</p>` (the wizard's "Step N of M" `.help` idiom).
- Group 1, small heading `Your bot` (`<h2>`): Bot token, Owner Telegram user ID.
- Group 2, small heading `Your console login` (`<h2>`): Dashboard username, Dashboard password.
- Optional fields move into one collapsed disclosure under them:
  `<details><summary>Optional — add later in Settings</summary> Default channel, Crypto Pay token </details>`.
  Inside it, each keeps its existing help line plus "You can add this any time in Settings → Bot connection."
- The BotFather `<details open>` instructions stay as-is — this is the best copy on the page. Reuse
  the same 5-step list verbatim in Settings (F2.2).

**F1.4 — Field copy doesn't explain the jargon in place.**
"Owner ID" and "Default channel" assume knowledge the first-run owner doesn't have yet.

*Prescription.* Exact help strings (replace the current ones):

- Bot token (keep, tighten): `From @BotFather. Stored privately on this machine (.env) and never shown to anyone.`
- Owner Telegram user ID: `Your own Telegram account number — message @userinfobot and it replies with it. This account controls the whole console.`
- Default channel (inside the optional group): `Where posts and paid access go when nothing else is set, for example @myshopnews.` plus `You can add this any time in Settings → Bot connection.`
- Crypto Pay token (inside the optional group): `From @CryptoBot → Crypto Pay → Create App. Lets the console verify payments automatically.`
- Dashboard password (keep): `At least 6 characters — this unlocks the console.`

Give every input an explicit `for`/`id` pair (`setup-owner-id`, `setup-default-channel`,
`setup-web-username` are currently implicit-label only) — see §5.

**F1.5 — "Skip for now" is a one-way trap.**
`setup_skip` permanently writes `setup_done=1`; the first-run gate never comes back. A stray tap
on the secondary button silently ends onboarding with no warning and no statement of where to
resume.

*Prescription.* Keep the button (it is a legitimate path) but make it honest:

- Copy under the button: `You can finish setup later in Settings → Bot connection. This page won't come back.`
- Add `onclick="return confirm('Finish setup later in Settings → Bot connection? This page won't come back.')"` — the same progressive-enhancement confirm the MFA/Bots pages already use; without JS the skip still works.

**F1.6 — Success page under-explains the restart and oversells readiness.**
After `setup_submit` succeeds, the page says "Restart the application…" and offers "Go to sign
in" — correct (the owner web account exists in the DB), but a non-technical owner doesn't know
*how* to restart, and doesn't know what works now vs. after restart.

*Prescription.* Exact structure for the success body:

```
<div class="alert alert-success" role="status" aria-live="polite">Setup complete — your login works right now.</div>
<p>The bot itself starts after one restart. The running app keeps the settings it started with.</p>
<p class="help">Desktop: quit and reopen. Server (systemd): <code>systemctl restart telegram-agent</code>. Docker: <code>docker compose restart</code>.</p>
<p class="help">After restarting, sign in and follow the dashboard checklist: add a plan, then share your bot with customers.</p>
<p><a class="btn" href="{login}">Go to sign in</a></p>
```

Per-platform restart text must match `restart_required_banner` (`src/web/dashboard.rs`) word for
word — one source of truth for the restart story.

### F2 — Dashboard settings (`src/web/settings.rs`: `connection_panel`, `management_panel`, `mfa_panel`, `settings_save`)

**F2.1 — "0600 permissions" and half-messages.**
The bot-token help reads "…Stored in .env with 0600 permissions and never rendered back." —
"0600 permissions" is file-mode jargon; a seller can't act on it. The token field's help also
opens mid-sentence: "leave blank to keep the current token" (lowercase, no context).

*Prescription.* Exact strings for `connection_panel`:

- Token help, not connected: `From @BotFather. The bot connects on restart once a token is saved. Get one in two minutes: message @BotFather, send /newbot, paste the reply here.`
- Token help, configured: `Leave blank to keep the token you already saved — the console never shows it. To replace it, get a new token from @BotFather and paste it here.`
- Replace "0600 permissions" everywhere with `stored privately on this machine`.
- Owner ID help: `Your own Telegram account number (@userinfobot replies with it). Leave blank to keep the current one.`
- Default channel help (keep the blank-keeps-current contract): `Where posts and paid access go when nothing else is set — for example @myshopnews. Leave blank to keep the current one.`
- Crypto help: `From @CryptoBot → Crypto Pay → Create App. Lets the console verify payments automatically. Leave blank to keep the current token.`
- Closing help line (keep, unify wording): `Blank fields change nothing. Saving writes the .env file — changes go live when the app restarts.`

**F2.2 — A returning owner who lost their token has no recovery pointers.**
Setup has the BotFather recipe; Settings has none. If the token was revoked, the owner is stuck.

*Prescription.* Under the token field in `connection_panel`, add the collapsed recipe:
`<details><summary>How to get a bot token (2 minutes)</summary>` containing the same 5-step
`<ol>` used on the setup page (keep the two in sync — extract nothing across files; duplicating
the static string in both files is acceptable for this pass).

**F2.3 — "Run setup bot" is a mystery button.**
In `management_panel`, `Run setup bot` pushes commands/description via `settings_setupbot`. No
owner can guess that. This is also the panel's only action with no success expectation set.

*Prescription.* Label: `Push commands & profile to Telegram`. Add
`<p class="help">Updates your bot's command list and description on Telegram. Use this after changing the bot's profile.</p>`
The success/failure banners in `settings_page` (`?setupbot=1`) already have the right shape — keep
them.

**F2.4 — Two MFA inputs share one accessible name.**
`mfa_panel` renders two separate forms (disable, regenerate) whose code inputs are both labelled
"Current authenticator or recovery code". Speech input and screen-reader rota become ambiguous,
and a non-technical owner can't tell which button pairs with which field.

*Prescription.* Distinct visible labels: `Code — to disable MFA` and `Code — to rotate recovery
codes`. Keep the `one-time-code` autocomplete and the existing help sentences.

**F2.5 — Error copy lacks the "what to do" half.**
`settings_save` errors say e.g. "Invalid bot token (Telegram rejected it). Nothing was saved." —
the shape is right (what happened + what was NOT changed) but the recovery step is missing.

*Prescription.* Extend to the full §4.4 shape:
`Telegram rejected that bot token. Compare it against the message @BotFather sent you — it looks like 123456:ABC-DEF…. Nothing was saved.`
`Owner ID must be the number @userinfobot replies with — for example 123456789. Nothing was saved.`
(Swap `validate_bot_token` for `validate_token_username` here too so Telegram's reason can be
included; network-failure copy as in F1.2.)

**F2.6 — What already works, keep it.** Blank-keeps-current, write-only secrets, badges
(configured / not set), the `restart_required_banner` at the top of the page, and the success
banner "Saved. The bot connects on restart." are all correct — do not regress them.

### F3 — Bot management (`src/web/admin_console.rs`: `render_add_page`, `bots_add_page`, `bots_add_validate`, `bots_add_submit`, `bots_add_snippet`, `bots_page`, `bots_save`, `bots_disable`, `bots_delete`) — the most complicated surface

**F3.1 — The "wizard" shows both steps at once and Step 2 works without Step 1.**
`render_add_page` always renders "Step 1 — Validate token" *and* "Step 2 — Add bot"; the hidden
`validated=1` field is hardcoded, so nothing actually gates Step 2. A user can submit garbage
straight to Step 2 and meet the key-pattern error first — the worst possible first contact.

*Prescription.* Gate the steps: `render_add_page` takes
`validated_username: Option<&str>` (it already does). Render Step 2 **only when**
`validated_username.is_some()`; otherwise render Step 1 plus a locked-step teaser:

- Step 1 heading stays, but use the wizard idiom: `<p class="help">Step 1 of 2 — prove the token works</p>`.
- Step 2: `<p class="help">Step 2 of 2 — describe the bot</p>`, and pre-fill from the validated
  username: `username` = `@{u}`, `name` = `{u}`, `key` = `{u}` (sanitized to the slug charset).
- Validation success alert (keep): `Token valid: @{u}` — extend to
  `Token valid — this is @{u}. Now give the bot its console details below.`

**F3.2 — Step 2 is pure jargon with zero help: Key, Token env var, Owner user ID, Default channel ID.**
A non-technical owner cannot invent a TOML key or an env-var name, and nothing says what happens
to the token itself.

*Prescription.* Exact labels + help for the Step 2 form:

- `Key` → label `Bot nickname` — help: `A short name for this bot, used only inside this console. Lowercase letters, numbers, dashes — for example main.`
- `Username` → keep, help: `Optional — the bot's @username on Telegram, filled in for you after validation.`
- `Name` → keep, help: `Optional — how this bot is named in lists here. Defaults to the nickname.`
- `Owner user ID` → help: `Your own Telegram account number (@userinfobot replies with it). This bot's customers and channels belong to this account.`
- `Default channel` → help: `Optional — where this bot posts and hands out paid access when nothing else is set. Either @channelname or the numeric chat ID.`
- `Default channel ID` (numeric) → **drop from the form.** Two channel fields for one concept is
  a proven confusion; `default_channel_value()` in this file already prefers
  `default_channel` and falls back to `default_channel_id`, so a single field suffices. (No
  handler-signature change required to accept one of them empty; if the web layer prefers zero handler
  churn, hide the second field with a `.help` note instead: `You almost never need this — use the field above.`)
- `Token env var` → label `Where the token will be stored` — help: `The setting name in your config file that will hold this bot's token, for example BOT_TOKEN_MAIN. The console never stores the token itself — you paste the actual token into .env (or your bot config file) yourself.`
  Pre-suggest a value: `BOT_{KEY uppercased with - and _ folded to _}` (e.g. key `main` →
  suggestion `BOT_MAIN`) as the input's `value` when empty.
- Submit button: `Add bot` (keep).

**F3.3 — The fallback path dead-ends into a raw file download.**
When `BOTS_CONFIG` is unset or unwritable, `bots_add_submit` silently redirects to
`/bots/add/snippet`, which serves a `text/plain` TOML attachment. The user gets an unexplained
download with no return path and no instructions — the single most likely dead end for a fresh
install (most owners won't have `BOTS_CONFIG` set).

*Prescription.* Keep the snippet route and the download (no new routes). Add `snippet: Option<String>`
to `BotsQuery`; on fallback, redirect to `/bots/add?snippet=1`. In `render_add_page`, when the
flag is set, read `LAST_BOT_SNIPPET` and render an HTML panel:

```
<div class="alert alert-warn" role="status" aria-live="polite">This bot couldn't be added automatically — your console has no bot config file yet. Two minutes, two steps:</div>
<p><b>1.</b> Copy the block below into a file called <code>bots.toml</code> next to your <code>.env</code> (create it if it doesn't exist).</p>
<code class="copyable">{toml_block}</code>
<p><b>2.</b> Tell the console where the file is: add <code>BOTS_CONFIG=/path/to/bots.toml</code> to <code>.env</code>, then restart the app. Your bot starts polling after the restart.</p>
<p><a class="btn btn-secondary" href="{/bots/add/snippet}">Download this block as a file</a>
<a class="btn btn-secondary" href="{/bots}">Back to Bots</a></p>
```

`code.copyable` (select-all on click) already exists in `assets/app.css`. The add page keeps its
Step 1 above the panel so the owner can immediately add the *next* bot once the file exists.

**F3.4 — `bots_save` fails and succeeds silently.**
Invalid `bot_id`/owner parse → bare redirect to `/bots`, no banner: the owner clicks Save and
nothing appears to happen. Successful save → also a bare redirect: no confirmation, and no
restart consequence stated (the DB `bots` row is boot-loaded).

*Prescription.*

- On parse failure or unknown bot: re-render `/bots` with `focused_error_alert("That bot no longer exists or the owner ID isn't a number. Nothing was changed.")` (unknown bot may keep its 404 page — pick one and be consistent; the banner re-render is friendlier on mobile).
- On success: redirect to `/bots?saved=1` and render, in the `added/disabled/deleted` banner block: `<div class="alert alert-success" role="status" aria-live="polite">Bot settings saved — they apply on restart.</div>`
- Set `state.db.set_config("restart_required", "1")` on every successful `update_bot_settings`.

**F3.5 — Danger actions: unlabeled consequences, ID-addressed confirms, inconsistent banners.**
The Disable and Delete buttons sit stacked with equal visual weight, both `btn-danger`, both bare;
Delete's `confirm()` names the bot by numeric ID and speaks in config jargon; the delete banner
("Bot {id} deleted.") omits the restart/resurrection truth that its own confirm dialog admits.

*Prescription.*

- Order per card, one row of actions after the save form: `Save` (primary, stays in the card form) → `Disable` (`btn btn-sm btn-secondary` — it is reversible; danger color is for destructive) → `Delete` (`btn btn-sm btn-danger`, last).
- Help line above the action row: `Disabling is reversible — the bot stays listed, it just stops after the next restart. Deleting removes its settings from the console.`
- Disable confirm: `Disable {key}? It stops responding after the next restart. You can enable it again any time.`
- Delete confirm: `Delete {key}? Its settings are removed from the console. Bots that come from your config file will reappear on restart unless you remove them from the file.`
- `bots_disable` and `bots_delete` must set `restart_required` (they don't today) and their PRG
  banners must follow §4.3: disable → `Bot {key} disabled — it stops responding after the next
  restart.` (alert-warn); delete → `Bot {key} deleted from the console. Bots from your config file reappear on restart unless removed there.` (alert-warn, not success).
- Banner copy must use the bot **key**, not the numeric id, everywhere (`bots_page` already loads the rows; resolve id → key for the banner).

**F3.6 — Card copy and the token hint.**
The card's bottom help is a run-on paragraph of config jargon; the token hint chip reads
"configured in config" / "not set" with no explanation; the card form's two inputs have no
recovery guidance.

*Prescription.* Exact strings:

- Token hint chip: keep `secret-hint`, `aria-label="Token hint"`, but render text as `token …{last4}` when present, `token not saved yet` when empty, `token in config file` when no BotContext matches.
- Card help (replaces the run-on): `Tokens are stored only in your config file (.env / bots.toml) and are read once at startup — this console never shows or edits a token. Owner and channel changes here apply on restart.`
- Owner user ID help: `Your own Telegram account number (@userinfobot replies with it).`
- Default channel ID help: `Numeric chat ID of the channel (starts with -100…). Leave blank if this bot has no default channel.`
- Keep explicit `for`/`id` on both card inputs (unique ids per card: `bot-owner-{id}`, `bot-channel-{id}`) — §5.

**F3.7 — Empty state points at env vars first.**
"No bots configured. Use Add bot or add an entry to BOTS_CONFIG / BOTS_JSON (or env) and
restart." — jargon-first, and it hides the reassuring fact that the primary bot is already
running.

*Prescription.* Keep `.empty` shape, new copy:
`<b>No extra bots yet.</b> Your primary bot is already running from .env — most owners start with one. Add another bot here only when you need a second one.`
with the existing `Add bot` button link as the single CTA. Keep the env-var fallback as a second
sentence only if `list_bots()` is empty *and* the primary context exists — otherwise keep the
current config-file sentence as the trailing sentence.

**F3.8 — Step-indicator style drift.**
This page says "Step 1 — Validate token"; the identity wizard says "Step N of M" in a `.help`
line. Pick one house style.

*Prescription.* House style = the wizard's: `<p class="help">Step 1 of 2 — …</p>`. No numbered
chips, no new components.

### Context surfaces — no changes requested
`src/web/wizard.rs` (identity wizard) is the pattern donor, not a target. `restart_required_banner`
(`src/web/dashboard.rs`) is canonical and stays word-for-word as-is.

---

## 2. Cross-cutting rules (bind every present and future surface)

**R1 — Restart honesty.** Any POST that changes what the app reads at boot (`.env` writes,
`bots.toml` writes, DB `bots` rows) must: (a) set `restart_required`; (b) state the restart in the
immediate success banner with the exact phrase `… apply on restart.` / `… after the next
restart.`; (c) never offer a "restart" button or imply hot-reload. Per-platform restart commands
appear only in `restart_required_banner` and the setup success page, word-for-word identical.
Routinely-live things (dashboard password, sidebar prefs, MFA) never mention restart.

**R2 — Token and status display.** Tokens are write-only: `secret-field` + `SECRET_TOGGLE` for
entry, never a value rendered back, never logged. Configured-ness is a `badge badge-success`
"configured" / `badge badge-warning` "not set". The only token echo allowed is the last-4 hint
chip (`…{last4}`), labelled `aria-label="Token hint"`. Status never rides on color alone — badge
text is always the word.

**R3 — Danger actions.** Order within any card: primary (Save) → reversible (Disable) →
destructive (Delete). Destructive = `btn-danger` + a `confirm()` that names the object in plain
language (key, not id), states the consequence, and states what survives. After the PRG, the
banner repeats what changed and what was NOT changed. Disabling a thing is reversible and therefore
**not** danger-colored.

**R4 — Empty states.** `.empty` with a `<b>` headline in plain language, one sentence of
reassurance, one primary action. Never lead with env-var names; jargon may appear only as the
trailing fallback sentence.

**R5 — Error shape.** Every error alert = [What happened] . [What to do, with the concrete
example] . [Nothing was saved/changed.] Rendered by `focused_error_alert` (so it takes focus) and
re-rendered **in place** with all non-secret inputs preserved — never a "Back to X" link that
discards a form. Success/info banners: `role="status" aria-live="polite"`; errors:
`role="alert"`.

**R6 — Copy voice.** No acronym without an in-place expansion ("bot token — the password
Telegram gave your bot"). Buttons are verb + object (`Add bot`, `Save settings`, `Push commands &
profile to Telegram`), never "Submit"/"OK". IDs and keys render in `.mono`. Every optional field
says "Optional —" and where to add it later.

---

## 3. Accessibility floor (WCAG 2.1 AA — non-waivable, applies to every checklist item)

1. **Labels:** every input has an explicit `<label for>`/`id` pair; implicit label-wrapping is
   grandfathered only where listed as keep, and new/edited markup uses explicit pairs. The two MFA
   code inputs get unique accessible names (F2.4).
2. **Focus:** errors land on a `tabindex="-1" data-focus-on-load` alert (`focused_error_alert`) so
   screen readers announce the failure; focus order follows DOM order (single column, no
   reordering tricks).
3. **Keyboard:** no JS-only path — `confirm()` guards and secret toggles degrade; `details`
   elements are native; every form is a real POST.
4. **Live regions:** success/info banners `role="status" aria-live="polite"`; error banners
   `role="alert"`; banner placement is the first content in `<main>` so the announcement precedes
   page content.
5. **Contrast:** no new colors — existing tokens only (`--ink-3` help text, `--warn`,
   `--danger-tint` alerts are pre-verified ≥ 4.5:1 in `app.css`'s header audit). Banners must use
   `alert-*` classes, never inline styles.
6. **Touch targets:** keep ≥ 44px on small screens (existing `@media (max-width: 900px)` rule);
   the stacked Disable/Delete buttons in a card must remain separate tap targets ≥ 44px apart.
7. **Semantics:** bot cards are `<article>`; the token hint is text with `aria-label`; tables keep
   their `table-wrap` region pattern; headings stay one h1 per page, h2 for grouped sections (F1.3).

---

## 4. Implementation checklist for web (ordered; execute top to bottom)

No new routes, no `schema.sql`/`Db` signature changes, no security-model changes. Each item
compiles and passes `cargo clippy -- -D warnings` independently.

1. **`src/web/channel_ops.rs` — `setup_page` + new `setup_form_body`.** Extract the form into a
   builder taking `alert: Option<&str>` + preserved non-secret values; apply F1.3 (two h2 groups,
   optional `<details>`, step help line), F1.4 strings, explicit `for`/`id` on all inputs.
2. **`src/web/channel_ops.rs` — `setup_submit`.** Local validation first, all errors in one
   `focused_error_alert` re-rendered via the builder (F1.1/F1.2); swap `validate_bot_token` for
   `validate_token_username` (`pub(super)` in `src/web/admin_console.rs`); adopt the F1.2 strings;
   apply the F1.6 success body (restart commands matching `restart_required_banner`).
3. **`src/web/channel_ops.rs` — `setup_skip`.** Add the confirm `onclick` + the "This page won't
   come back" help line (F1.5).
4. **`src/web/settings.rs` — `connection_panel`.** F2.1/F2.2 copy pass: plain-language help,
   collapsed BotFather recipe, "0600" removed, blank-keeps-current preserved; keep badges.
5. **`src/web/settings.rs` — `settings_save`.** Use `validate_token_username` for reason-carrying
   errors; extend error strings to the §R5 shape (F2.5).
6. **`src/web/settings.rs` — `management_panel` + `mfa_panel`.** Rename `Run setup bot` →
   `Push commands & profile to Telegram` + help (F2.3); unique MFA labels (F2.4).
7. **`src/web/admin_console.rs` — `render_add_page`.** Gate Step 2 on `validated_username`,
   wizard-style step lines, pre-fill username/name/key, token-env suggestion, full help strings
   (F3.1/F3.2).
8. **`src/web/admin_console.rs` — `bots_add_submit`.** Fallback redirects to
   `/bots/add?snippet=1`; add `snippet` to `BotsQuery`; render the HTML snippet panel with
   `code.copyable` + two-step instructions in `render_add_page` (F3.3). On successful config
   write: `set_config("restart_required", "1")` (currently missing).
9. **`src/web/admin_console.rs` — `bots_page`.** Banner block: add `?saved=1` handling; switch
   banners to key-addressed §R3 copy; apply card copy/token-hint/field-help strings (F3.5/F3.6);
   new empty-state copy (F3.7); explicit `for`/`id` per card input.
10. **`src/web/admin_console.rs` — `bots_save`.** Stop silent redirects: error banner with
    "Nothing was changed." on parse/unknown; `?saved=1` PRG + `restart_required` on success (F3.4).
11. **`src/web/admin_console.rs` — `bots_disable` / `bots_delete`.** Set `restart_required`; key-
    addressed banners (alert-warn); confirm() copy naming `{key}` (F3.5).
12. **`assets/app.css` — expected: no changes.** All prescriptions reuse existing classes
    (`alert`, `help`, `secret-field`, `code.copyable`, `empty`, `badge`). If web finds a genuine
    token gap (e.g. the snippet `<code class="copyable">` inside a `details` needing no extra
    chrome), stop and file it back to design — do not add component CSS unilaterally.

**Review gate (design):** after items 1–11, run the manual smoke from `AGENT.md` §6 on a phone-width
viewport plus a keyboard-only pass: tab order reaches every field, each error re-render announces
itself, every success banner names its restart consequence.
