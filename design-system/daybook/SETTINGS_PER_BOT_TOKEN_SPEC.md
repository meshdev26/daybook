# SETTINGS_PER_BOT_TOKEN_SPEC — one token field per bot, honest saves, logout out of the rail

Status: **READY FOR WEB/SERVER** · Owner-mandated (verified 2026-11-01: the Settings
"Bot token" field writes a shadowed variable and its success banner lies; the
rail foot crams three controls).
Sources of truth: `DESIGN.md` + `assets/app.css`. Format follows
`MANAGEMENT_DISCLOSURE_SPEC.md` (audit table → exact markup → tests → budget).

Owner requirements (verbatim intent):
1. Settings must accept a token **per bot** — one write-only field per bot that
   exists in Operate → Bots.
2. "Log out" leaves the rail foot; it lives in Settings → Management.
3. Every save banner tells the truth about what was written and what a restart
   will do.

Zero DB changes, zero new dependencies, zero new routes except the one-token
`POST` arm on the existing logout route (§7). Tokens stay write-only: the
console never renders a current token, in any mode, on any surface.

---

## §0 Audit evidence (read before write)

| Fact | Evidence |
|---|---|
| Settings "Bot token" field posts `bot_token` to `POST /settings/save`, which writes the legacy key | field: `src/web/settings.rs:264-265`; handler push: `src/web/settings.rs:471` `updates.push(("TELEGRAM_BOT_TOKEN"…))` |
| `TELEGRAM_BOT_TOKEN` is **shadowed** whenever `BOTS_CONFIG`/`BOTS_JSON` parses to ≥1 bot — the file branch returns before the legacy branch is ever read | `src/config.rs:503-535` — BOTS_CONFIG win: `:505-511`; BOTS_JSON: `:514-520`; legacy env read only at `:522-534` |
| The "Saved. The bot connects on restart." banner is therefore false in multi-bot mode | `src/web/settings.rs:513`; shadow chain above |
| The field is ambiguous ("which bot?") — badge derives from `cfg.owner_id`, not from any token | badge heuristic `src/web/settings.rs:231-235` (`owner_id == 0`); `SharedConfig::owner_id` is the *primary bot's* owner in multi-bot mode: `src/config.rs:481,496` |
| core prerequisite landed: `BotConfig.token_env: Option<String>` | `src/config.rs:32-46` (field at `:45`); parsed from bots.toml `:629-637`, BOTS_JSON `:576-584`; legacy fallback synthesizes `Some("TELEGRAM_BOT_TOKEN")` `:540-553` (`:551`) |
| **`state.cfg.bots[i].token_env` does not compile today** — `WebState.cfg` is `Config = SharedConfig`, which has no `bots` field | `src/web/mod.rs:143` (`pub cfg: Config`); alias `src/config.rs:56` (`pub type Config = SharedConfig`); struct `src/config.rs:7-30` |
| `write_env_file` preserves only keys **outside** `SETUP_ENV_KEYS` (16 fixed keys, incl. `TELEGRAM_BOT_TOKEN`, excl. every `BOT_*` var) | writer `src/config.rs:193-198`; preserve loop `:212-222`; key list `:150-167` |
| Writing a `BOT_*` var through `write_env_file` would **duplicate** an existing line (old line preserved + new line written); dotenvy never overrides an already-set var, so the **stale first line would win at boot** | duplicate mechanism: `src/config.rs:212-222` (old `BOT_*` line lands in `preserved`) + `:225-227` (update written anyway); first-wins: `load_dotenv` doc `src/config.rs:135`, `dotenvy::dotenv()` at `:137` |
| The per-call managed-key writer exists precisely to avoid that: replaces its managed keys, preserves everything else, same atomic tail | `write_env_file_replacing` `src/config.rs:245-290` (doc `:236-244` names the duplicate hazard; core `:254-290`; preserve loop `:271-287`); atomic tail `:301-345` |
| Blank-keeps-current is a single helper; the save writes only non-blank fields and refuses an all-blank submit | `non_blank` `src/web/settings.rs:442-444`; updates built only for non-blank `:463-491`; nothing-to-save `:492-502` |
| Per-token validation (getMe verdict → copy) already exists and is shared | `validate_token_username` `src/web/bots_add.rs:80` (hardcoded `https://api.telegram.org/bot{token}/getMe` at `:81` — no URL seam, a deliberate exfil guard); `token_error_copy` `:73-79` (both verdicts end "Nothing was saved.") |
| `restart_required` is set once per .env-writing save and read by a standing banner | set: `src/web/settings.rs:508`; banner: `src/web/dashboard.rs:8-17`; settings renders it at `settings.rs:164` |
| Operate → Bots lists **DB rows** (`db.list_bots()`), not config entries | `src/web/bots_admin.rs:182`; `Db::list_bots` `src/db.rs:1799`; `BotRecord` (key, name, owner, …) `src/db.rs:194-204` |
| DB rows are seeded once at boot from the resolved config — and **only when the table is empty** (later hand-edits to bots.toml never backfill) | `src/main.rs:235` → `Db::backfill_bots_at_startup` `src/db.rs:2075-2105` (early return `:2076-2078`) |
| The boot manager loads only enabled bots **with** token+owner; a tokenless bot has **no** `BotContext` | `src/main.rs:327-343` (skip at `:330`) |
| Bots-card token badge today: `BotManager.get(id)` → teloxide `bot.token()`; last-4 echo; `None` → "token in config file" | `src/web/bots_admin.rs:236-251` |
| Bots-card token copy claims "this console never shows **or edits** a token" — already false in legacy mode (Settings edits `TELEGRAM_BOT_TOKEN`) and about to get worse | `src/web/bots_admin.rs:343` vs `src/web/settings.rs:471` |
| Rail foot renders "Settings · Log out · Theme" in one cramped line | `src/web/html.rs:225` (the brief's "html.rs:205" has drifted; the markup is at `:225`) |
| **There is no `POST /logout` in the web console** — the route is `GET /logout`; only the REST API has a logout POST, on a different auth surface | web: `src/web/mod.rs:240` (`.route("/logout", get(logout))`) → handler `src/web/auth_pages.rs:374-397`; REST: `src/api.rs:344`, route `:4474` (`/api/v1/auth/logout`) |
| The logout handler is method-agnostic (reads only headers), deletes the session by cookie token, clears the cookie, 303-redirects to /login | `src/web/auth_pages.rs:374-397`; axum `Redirect::to` is 303 (correct after POST) |
| Session cookie is `SameSite=Strict`, so a cross-site GET logout carries no cookie and no-ops | cookie builders `src/web/auth_pages.rs:357-371` |
| Settings tests live in **`src/web/tests/settings.rs`** (the brief's "tests/settings.rs" is the integration dir, which holds only `web_base_path.rs`) | `src/web/tests/settings.rs` (319 lines); `tests/` listing |
| Test pins that must survive | blank-save: `src/web/tests/settings.rs:37-56`; legacy banner byte-pin "Saved. The bot connects on restart.": `:79`; invalid-token no-write: `:90-112`; owner error: `:116-140`; section title "Settings → Bot connection" pinned by setup test: `src/web/tests/setup.rs:112`; setup write-only pin `name="bot_token" value=`: `src/web/tests/setup.rs:150`; logout GET route test: `src/web/tests/misc.rs:122-129` |
| No test pins the rail foot "Log out" or `.secret-field` markup | grep over `src/web/tests/*`, `smoke_tests.rs`, `security_tests.rs` — clean |
| Positive-path token tests cannot hit Telegram: validation URL is hardcoded and no test mocks getMe | `src/web/bots_add.rs:81`; grep of `tests/bots.rs` for `getMe|MockServer|api.telegram` — empty; existing settings tests deliberately avoid the token path (`tests/settings.rs:67-68`) |
| The env-writer test seam is `ENV_FILE` + `ENV_LOCK` | `src/web/test_support.rs:44-56`; usage `src/web/tests/settings.rs:41-55` |
| Form extraction is `axum::Form` → **serde_urlencoded**, which does not support `#[serde(flatten)]`; no `axum-extra`/`serde_html_form` in the tree | `Cargo.toml:41-49`; no flatten usage in `src/` |
| SharedConfig struct literals that a new field breaks (compile) | `src/config.rs:483` (production, updated by the change itself), `src/lib.rs:86`, `src/bots.rs:156`, `src/handlers/commands/tests.rs:134`, `src/web/test_support.rs:14-29` |
| House idioms this spec reuses (do not invent new ones) | `.secret-field` + `SECRET_TOGGLE`: `src/web/mod.rs:456`, `assets/app.css:350-362`; badge family `assets/app.css:478-496`; alert family `assets/app.css:499-504`; `<details open><summary>` panels `src/web/settings.rs:260,292,314,341`, rules `assets/app.css:532-548`; `esc()` mandatory `src/web/mod.rs:185-191`; display-name sugar `bot_display_name` `src/web/mod.rs:505-507`; button styles `assets/app.css:406-416` |
| `settings.rs` is 639 lines — the 800 budget is real (bots_admin hit 1128 and was frozen report-only) | `wc -l src/web/settings.rs`; budget flag precedent `design-system/daybook/VISUAL_POLISH_SPEC.md:459-468,578` |

---

## §1 Plumbing prerequisite (P0 — one-time, makes the brief's access path real)

The brief specifies reads via `state.cfg.bots[i].token_env`. That path does not
compile today (§0). Make it true with the **smallest possible** config-layer
change; do **not** add a `WebState` field (that would touch 7 struct literals —
`main.rs:851`, `embed.rs:131`, `web/tests/mod.rs:46`, `mcp/mod.rs:700`,
`web/test_support.rs:38`, `api/tests.rs:104`, `web/tests/setup.rs:49` — instead
of 4 test literals).

**P0a — `SharedConfig` carries the boot snapshot** (`src/config.rs:7-30`):

```rust
/// Boot snapshot of the resolved bots — a mirror of `AppConfig.bots` so the
/// web layer can address per-bot `token_env` (and resolved-token presence)
/// through the state it already has. Populated only by `AppConfig::from_env`;
/// test literals leave it empty (legacy mode).
pub bots: Vec<BotConfig>,
/// True when `resolve_bots` fell through to the legacy
/// `TELEGRAM_BOT_TOKEN`/`OWNER_ID` pair — i.e. no `BOTS_CONFIG`/`BOTS_JSON`
/// source parsed to at least one bot (`src/config.rs:503-534`). Decides which
/// connection panel the Settings page renders (§2 mode matrix). Source-tracked
/// at resolution time so a *set-but-broken* BOTS_CONFIG honestly renders as
/// legacy (that is what the running process actually uses).
pub bots_legacy: bool,
```

`AppConfig::from_env` (`src/config.rs:440-501`) populates both from the same
local: `shared.bots = bots.clone()`; `AppConfig.bots` stays canonical for
`main.rs` readers (`main.rs:235,261,329`). `SharedConfig` clone cost stays
negligible (a handful of small strings per request extraction).

**P0b — `resolve_bots` reports its source** (internal, one caller at
`src/config.rs:480`):

```rust
fn resolve_bots() -> (Vec<BotConfig>, bool /* from_files */) { … }
```

`from_files = true` only for the BOTS_CONFIG/BOTS_JSON branches
(`config.rs:505-520`); the legacy branch (`:522-534`) returns `false`.
No drift: the flag is decided where the decision already happens.
Add one unit test beside `legacy_fallback_bot_carries_telegram_bot_token_env`
(`config.rs:1107-1120`) asserting the tuple for each branch (the toml/json
branches are testable via the existing `parse_bots_toml`/`parse_bots_json`
tests; the legacy branch via `legacy_primary_bot`).

**P0c — update the four test literals** (compile fix, mechanical):
`bots: Vec::new(), bots_legacy: true` in `src/lib.rs:86`, `src/bots.rs:156`,
`src/handlers/commands/tests.rs:134`, `src/web/test_support.rs:14-29`.
`true` = legacy mode, which is exactly what every existing settings test
assumes — so every pin in §8's at-risk list keeps passing unchanged.

---

## §2 Mode matrix

| | **Multi-bot mode** | **Legacy (single-bot) mode** |
|---|---|---|
| When | `BOTS_CONFIG` or `BOTS_JSON` parsed to ≥1 bot (`config.rs:505-520`) → `cfg.bots_legacy == false` | Nothing parsed → legacy env pair (`config.rs:522-534`) → `cfg.bots_legacy == true`. A *set-but-broken* file honestly lands here (the process really is running legacy) |
| Detection in code | `!state.cfg.bots_legacy` (helper §3.1) | `state.cfg.bots_legacy` |
| Connection panel | §3: mode line + one write-only row per **DB bot** + recipe + owner/channel (scoped note) + Crypto Pay | Today's panel **byte-identical** (`settings.rs:260-275` markup unchanged) + the §4 mode line inserted as the panel's first child |
| Rows source | `db.list_bots()` (`bots_admin.rs:182` precedent — must match Operate → Bots 1:1) joined by `key` with `cfg.bots` | n/a |
| Row badge source | `cfg.bots[i].token` emptiness (boot-resolved, `config.rs:36,630-637`; unset var → `""` at `:634`). Unmatched row falls back to the `BotManager` derivation already shipped at `bots_admin.rs:236-251` | Today's `owner_id == 0` heuristic, unchanged (`settings.rs:231-235`) |
| Token save target | `write_env_file_replacing` with managed set = exactly the keys written this save (§5) | `write_env_file` with `TELEGRAM_BOT_TOKEN` — unchanged (`settings.rs:471,503`) |
| Success banner | "Saved — restart to apply." + "Tokens set for: {names}." when ≥1 token written (§5.4) | Today's copy byte-identical (`settings.rs:513-514`; pinned at `tests/settings.rs:79`) |
| Token display | Never — write-only, placeholder only | Never — unchanged |

Mutual exclusivity is what kills the false banner: in multi-bot mode the legacy
`bot_token` field is not rendered at all, so `settings_save` can never write the
shadowed key from the UI. Belt-and-braces: the multi-bot branch of
`settings_save` **rejects** a posted non-blank `bot_token` with an error banner
("Single-bot token fields are not used in multi-bot mode — set each bot's token
below. Nothing was saved.") — a stale bookmark or crafted POST must not
resurrect the shadow write.

---

## §3 Prescribed markup — multi-bot connection panel

### §3.1 Helper placement (decided up front — see §9 budget)

New submodule **`src/web/settings_tokens.rs`** (move-only split idiom;
`mod.rs:52` declaration + `mod.rs:98` glob), housing ~115 lines:

- `pub(super) fn multi_bot_mode(state: &WebState) -> bool` — `!state.cfg.bots_legacy`.
- `pub(super) struct BotTokenRow { record, token_env: Option<String>, configured: bool }`
  and `pub(super) fn bot_token_rows(state: &WebState) -> Vec<BotTokenRow>` —
  `db.list_bots()` joined by `key` with `cfg.bots`; `configured` from
  `cfg.bots[i].token` emptiness, falling back to the
  `state.bots.get(id) → ctx.bot.token()` derivation (`bots_admin.rs:236-251`)
  for unmatched rows.
- `pub(super) fn token_env_updates(state: &WebState, form: &HashMap<String, String>)
  -> Result<(Vec<(&'static str, Option<&'static str>), Vec<String>), String>` —
  the pure composition unit §5 needs (network-free, unit-testable).

### §3.2 Exact markup (multi-bot branch of `connection_panel`)

Row variables: `{display_name}` = `bot_display_name` (`mod.rs:505-507`),
`{key}`, `{var}` = `token_env`, `{badge}` = §3.3, `{id}` = sanitized key
(`[A-Za-z0-9_-]` only — anything else degrades the row to state C; same guard
idiom as `js_key` at `bots_admin.rs:276-284`). Every dynamic value through
`esc()`.

```html
<details open id="connection"><summary>Bot connection</summary>
<article class="panel">
<p class="help" id="connection-mode"><b>Multi-bot mode.</b> Your bots come from
a bot config file (<code>BOTS_CONFIG</code> / <code>BOTS_JSON</code>). Each bot
has its own token field below; its card lives under <a href="{bots_url}">Operate → Bots</a>.</p>
<form method="post" action="{save_url}">
<input type="hidden" name="section" value="connection">

<!-- one block per DB bot, in list_bots() order — STATE A: token_env = Some(var) -->
<label for="settings-bot-token-{key}">Bot token — {display_name} {badge}</label>
<span class="secret-field"><input type="password" id="settings-bot-token-{key}" name="bot_{key}" autocomplete="new-password" placeholder="Paste a new token from @BotFather — the current one is never shown">{toggle}</span>
<span class="help">Saved to the <code>{var}</code> setting in your bot config file. Leave blank to keep the current one — the console never shows it.</span>

<!-- STATE B: token_env = None (inline token in the config file) — no input -->
<label>Bot token — {display_name} {badge}</label>
<span class="help">This bot's token is set directly in your bot config file — edit it there. The console cannot write it (there is no setting name to write to).</span>

<!-- STATE C: no config match (console-added bot pending restart, or hand-edited bots.toml not yet loaded) — no input -->
<label>Bot token — {display_name} {badge}</label>
<span class="help">The console can't see this bot's config entry yet. Restart to load your bot config, then set its token here.</span>

{recipe}
<label for="owner_id">Owner Telegram user ID</label>… <!-- unchanged, settings.rs:267 -->
<label for="default_channel">Default channel</label>…  <!-- unchanged, settings.rs:268 -->
{multi_bot_scope_note}                                  <!-- one line, below -->
<label for="settings-crypto-token">Crypto Pay token {crypto_badge}</label>… <!-- unchanged, settings.rs:269-270 -->
<button type="submit">Save settings</button>
</form>
<p class="help">Blank fields change nothing. Saving writes your config file — changes go live when the app restarts.</p>
<p class="help">AI provider and usage live on the <a href="{ai_url}">AI page</a>.</p>
<p class="help">REST API keys are managed on the <a href="{keys_url}">API keys</a> page.</p>
</article></details>
```

`{multi_bot_scope_note}` (honesty rider for the two legacy-env fields that the
file branch never reads — `config.rs:505-519` returns before `:522-534`):

```html
<span class="help">With a bot config file, each bot's owner ID and channel live on its <a href="{bots_url}">Bots card</a> instead; these two fields only apply in single-bot mode.</span>
```

`{recipe}` = `BOTFATHER_RECIPE_SETTINGS` (`settings.rs:228`), rendered **once**
after the rows (same "after the token field(s)" position as today,
`settings.rs:266`). Row fields keep the house rhythm: `label for=` →
`.secret-field` wrapper (sibling, never nested in the label —
`app.css:350-362`) → `span.help` (`settings.rs:264-265` shape). `SECRET_TOGGLE`
(`mod.rs:456`) in every wrapper. The last standing help line keeps today's
blank-keeps-current sentence (`settings.rs:273`) with ".env" generalized to
"your config file" (token_env vars live in .env in the common case, but the
console cannot know where the process env was assembled — do not overclaim).

### §3.3 Row badge

State A/B/C, reusing shipped badge classes (`app.css:478-496`):

- configured → `<span class="badge badge-success">configured</span>`
- not set → `<span class="badge badge-warning">not set</span>`

No last-4 echo on Settings rows (the Bots card keeps its `…{last4}` badge,
`bots_admin.rs:236-248` — a deliberate split: the Bots card echoes presence,
Settings offers replacement only; two surfaces, two secrets postures, no new
CSS).

### §3.4 Form field names and extraction

Per-bot fields post as `bot_{key}`. `SettingsForm` (`settings.rs:431-438`) is
**replaced** by `Form<HashMap<String, String>>` extraction in `settings_save`:
serde_urlencoded does not support `#[serde(flatten)]` (§0) and adding
`serde_html_form` for one handler violates the zero-new-deps house rule. Known
keys are read with a two-line local helper (`fn f(&form, "owner_id") -> Option<&str>`
+ `non_blank` at `settings.rs:442-444` unchanged); unknown keys are ignored —
a posted `bot_nosuchbot` can never select a bot that isn't listed (fail closed).
`SettingsPasswordForm`/`SidebarForm` are separate structs and untouched.

---

## §4 Prescribed markup — legacy mode (byte-identical + one line)

Keep `connection_panel`'s current body exactly as shipped (`settings.rs:260-288`)
and insert one line as the panel's first child (after `<article class="panel">`,
before the form):

```html
<p class="help" id="connection-mode">This console is in single-bot mode. The fields below configure that one bot.</p>
```

The first sentence is the owner-mandated exact copy. Nothing else moves: same
field, same `name="bot_token"`, same badge heuristic, same recipe position,
same "Saved. The bot connects on restart." banner (pinned at
`tests/settings.rs:79`). The badge heuristic (`owner_id == 0` for "not
connected", `settings.rs:231-235`) is a known wart — it keys on owner id, not
token presence — but it is legacy-mode-only and out of this wave's scope
(§10).

---

## §5 Save contract (`settings_save`, multi-bot branch)

1. **Compose** (pure, `token_env_updates` in `settings_tokens.rs`): for each
   `BotTokenRow` **with a `token_env`** (state A only), `non_blank(form["bot_{key}"])`
   yields `(token_env_var, Some(value))` + the bot's display name. Blanks write
   nothing (the `non_blank` contract, `settings.rs:442-444`). Legacy keys
   (owner_id / default_channel / crypto_token) join the same managed set with
   `Some(value)` exactly as today (`settings.rs:473-491`).
2. **Shared-variable guard**: two non-blank rows resolving to the *same*
   `token_env` var with different values → error banner
   "Two bots share the setting <code>{var}</code> — fix the bot config file first. Nothing was saved."
   (identical values collapse to one entry). Without this,
   `write_env_file_replacing` would emit two lines for one key and dotenvy's
   first-wins (`config.rs:135`) would silently keep one of them.
3. **Validate every candidate token before any write** (F2.5, same verdicts):
   `validate_token_username` (`bots_add.rs:80`) per token, in list order; first
   failure → `settings_error` with
   `"{display_name} — {token_error_copy(e)}"` (both verdict copies already end
   "Nothing was saved.", `bots_add.rs:76-78`). No partial writes, ever.
4. **One atomic write**: `crate::config::write_env_file_replacing(&managed)`
   (`config.rs:245-290`) with managed = exactly the keys being written.
   Why not the legacy writer for `BOT_*` vars: they are outside
   `SETUP_ENV_KEYS` (`config.rs:150-167`), so `write_env_file` would preserve
   the old line *and* add the new one — duplicate key, stale value wins at boot
   (`config.rs:135,212-222,225-227`; the replacing writer's own doc names this
   hazard, `config.rs:236-244`). Why one call for legacy keys too: the
   replacing writer's output is byte-identical to `write_env_file` for keys it
   manages (updates first, foreign keys preserved verbatim, comments dropped —
   compare `config.rs:212-222` with `:271-287`), and a second sequential
   full-file write would be a needless race window. Legacy mode keeps
   `write_env_file` untouched (mode exclusivity, §2).
5. **Flag + banner**: `state.db.set_config("restart_required", "1")` once
   (`settings.rs:508` unchanged), then PRG-in-place response:
   - tokens written: `<div class="alert alert-success" role="status" aria-live="polite">Saved — restart to apply. Tokens set for: {names}.</div>` —
     `{names}` = esc'd display names, `, `-joined, in row order.
   - no tokens (owner/channel/crypto only): same alert without the "Tokens set
     for:" sentence.
   - followed by today's explanatory `<p class="help">` (`settings.rs:514`)
     with ".env" generalized to "your config file".
   All-blank submits keep "Nothing to save — all fields were blank."
   (`settings.rs:492-502`, pinned at `tests/settings.rs:53`).
6. **Shadow-write rejection**: a non-blank `bot_token` posted in multi-bot mode
   → error banner (§2), no write, no flag.

---

## §6 Cross-reference — Operate → Bots cards (tiny touch, ~4 lines)

In `bot_card_form`'s honesty paragraph (`src/web/bots_admin.rs:343`):

- Amend the false clause "this console never shows or edits a token" →
  "this page never shows a token" (it is already false today via
  `settings.rs:471`, and §3 makes "edits" false in both modes).
- Append one mode-aware sentence to the same `<p class="help">`:

```html
<!-- multi-bot mode -->Token: set it in Settings → <a href="{settings_url}">{display_name}</a>.
<!-- legacy mode -->Token: set it in Settings → <a href="{settings_url}">Connection</a>.
```

`{settings_url}` = `state.url("/settings?section=connection")`;
`{display_name}` = `bot_display_name` (`mod.rs:505-507`), esc'd. The link
deep-link lands on the section tab (`settings.rs:209-213` already supports
`?section=connection`). Nothing else on the card moves; the file stays ~572
lines, far under its frozen budget (§0).

---

## §7 Log out relocation

**Verification result (owner asked for "the existing POST /logout route"):**
the web console has only `GET /logout` (`mod.rs:240` → `auth_pages.rs:374-397`);
the only POST logout is the REST API's `/api/v1/auth/logout` (`api.rs:344,4474`),
a different auth surface that must not be reused by the SSR console. The
Web handler is method-agnostic (reads headers only, `auth_pages.rs:374-397`)
and `Redirect::to` is 303 — correct after POST. Prescribed: **add the POST arm
to the existing route** (one token) and post a form from Management; keep the
GET arm so `tests/misc.rs:122-129` and any bookmarked link keep working.

1. Route (`src/web/mod.rs:240`): `.route("/logout", get(logout).post(logout))`
   — handler unchanged.
2. Rail foot (`src/web/html.rs:225`): remove the
   `<a href="{base}/logout">Log out</a>` anchor. Result:
   `<div class="foot"><a href="{base}/settings">Settings</a> <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()">Theme</button></div>`
   — two controls, no cram.
3. Management panel (`settings.rs:337-376`), appended after the
   "Push commands & profile" form (`:360-361`):

```html
<h3>Console session</h3>
<form method="post" action="{logout_url}"><button type="submit" class="btn btn-secondary btn-sm">Sign out</button></form>
<p class="help">Sign out of this console. You will need your dashboard password to sign back in.</p>
```

`{logout_url}` = `state.url("/logout")`. Fallback if the route change is vetoed:
the same block with `<a class="btn btn-secondary btn-sm" href="{logout_url}">Sign out</a>`
reusing GET — same posture as today's rail link, and `SameSite=Strict`
(`auth_pages.rs:357-371`) already neuters cross-site logout GETs. Button-styled
link has house precedent (`settings.rs:401`).

---

## §8 Tests

### §8.1 At-risk pins (must keep passing — enumerated)

| Pin | Where | Survival condition |
|---|---|---|
| All-blank save → "Nothing to save", no `.env` created | `src/web/tests/settings.rs:37-56` | Legacy-mode test state (P0c sets `bots_legacy: true`); copy unchanged (`settings.rs:492-502`) |
| Legacy save banner byte-pin "Saved. The bot connects on restart." | `src/web/tests/settings.rs:79` | Legacy branch byte-identical (§4) |
| Invalid token → "Nothing was saved.", `.env` untouched | `src/web/tests/settings.rs:90-112` | Legacy validation path unchanged (`settings.rs:468`) |
| Owner-ID error copy | `src/web/tests/settings.rs:116-140` | Untouched |
| Setup copy references "Settings → Bot connection" | `src/web/tests/setup.rs:112` | Section title/summary stays "Bot connection" (§3.2, §4) |
| Setup token write-only pin | `src/web/tests/setup.rs:150` | Setup page untouched |
| GET /logout clears session | `src/web/tests/misc.rs:122-129` | GET arm kept (§7.1) |
| Compile: SharedConfig literals | `lib.rs:86`, `bots.rs:156`, `handlers/commands/tests.rs:134`, `web/test_support.rs:14-29` | P0c mechanical fix |

### §8.2 Additive tests (all in `src/web/tests/settings.rs`; zero token
literals — reuse the house fakes `test-token-placeholder`
(`config.rs:1041`) and the shape-invalid `123456:definitely-not-a-token`
(`tests/settings.rs:100`); a test helper
`make_state_multi_bot()` clones `make_state()` then sets
`state.cfg.bots`/`bots_legacy` directly — `cfg` is a plain field, no Arc
(`web/mod.rs:143`))

1. `settings_multi_bot_renders_one_field_per_bot` — two DB bots
   (`primary`/"Primary" → `BOT_PRIMARY` configured, `proxy26_bot`/"Proxy26_Bot"
   → `BOT_PROXY26_BOT` not set); asserts one `name="bot_primary"` and one
   `name="bot_proxy26_bot"`, each wrapped in `.secret-field` with the toggle
   marker, the right `{var}` label in the help (`<code>BOT_PRIMARY</code>` /
   `<code>BOT_PROXY26_BOT</code>`), badges (`badge-success` / `badge-warning`),
   the "Multi-bot mode." line, and **no** `name="bot_token"` field.
2. `settings_multi_bot_inline_token_row_has_no_input` — a `token_env: None`
   row renders state B (no input, "edit it there" copy) — protects the
   write-target invariant (`config.rs:39-44`).
3. `settings_multi_bot_blank_save_writes_nothing` — all `bot_*` blank →
   "Nothing to save" + no `.env` (mirror of `:37-56`).
4. `settings_multi_bot_composes_right_vars` — unit test of
   `token_env_updates`: two non-blank + one blank row → managed set contains
   exactly `(BOT_PRIMARY, Some(_))`, not `BOT_PROXY26_BOT`; names list carries
   only the written bot. Network-free (§8.2 preamble).
5. `settings_multi_bot_writer_replaces_and_preserves` — seam test via
   `ENV_FILE`/`ENV_LOCK` (`test_support.rs:44-56`): pre-seed `.env` with
   `BOT_PRIMARY=stale-value` + foreign `WEB_BIND=0.0.0.0`; run the composed
   save through `write_env_file_replacing` with the handler's managed set;
   assert exactly one `BOT_PRIMARY=` line, the foreign key preserved. (The
   end-to-end happy path needs live Telegram — validation URL is hardcoded,
   `bots_add.rs:81` — so the write is proven at the same seam the existing
   tests use, `tests/settings.rs:60-86` precedent.)
6. `settings_multi_bot_invalid_token_writes_nothing` — shape-invalid fake →
   offline verdict is Network ("Couldn't reach Telegram…"), banner contains
   the bot's display name and "Nothing was saved.", no `.env`, no flag.
7. `settings_multi_bot_rejects_shadowed_legacy_field` — non-blank
   `bot_token=` posted in multi-bot mode → §2 rejection banner, no write.
8. `settings_legacy_mode_renders_single_field_and_mode_line` —
   `bots_legacy: true` → `name="bot_token"` present, "This console is in
   single-bot mode." present, no `bot_*` fields (byte-guard for §4).
9. `settings_logout_link_lives_in_management_not_rail` — GET /settings: the
   rail foot (`html.rs:225` shape) contains no `/logout`; the management
   section contains "Sign out" + "Sign out of this console."; POST /logout
   with a valid session still clears it (new POST arm, §7.1).

---

## §9 Size budget + placement

- `settings.rs`: 639 → **~700** projected (mode branch in `connection_panel`
  +~20; `settings_save` multi-bot branch +~35; HashMap accessors +~8;
  `SettingsForm` −8; Management block +~6). The three multi-bot units
  (`multi_bot_mode`, `bot_token_rows`, `token_env_updates`, ~115 lines with
  docs) live in the new `settings_tokens.rs` from day one (§3.1) — this is the
  "approaching 800" prescription applied preemptively; the
  move-only-split precedent is `bot_card_form` (`bots_admin.rs:306-310`).
- `bots_admin.rs`: 568 → ~572 (§6). `config.rs`: +~20 (P0a/P0b + test).
- Hard rule: if `settings.rs` lands over **780**, stop and move more (the
  budget that froze bots_admin at 1128 is the same one, §0).

---

## §10 Adjacent honesty debts (flagged, non-normative — schedule separately)

1. **`write_env_file` drops unwritten owned keys**: any `SETUP_ENV_KEYS` key
   present in `.env` but absent from a save's updates is silently dropped from
   the file (`config.rs:219` `continue` on the owned-key match; updates-only
   reconstruction at `:224-231`). Today a legacy-mode save of
   `owner_id` without re-pasting the token wipes `TELEGRAM_BOT_TOKEN` from
   `.env` on the next write. Masked in tests because they write fresh files
   (`tests/settings.rs:82-84`). The multi-bot path is immune by construction
   (§5.4); the legacy path keeps the behavior byte-for-byte per §4 and needs
   its own one-line fix + test.
2. **Setup page has the same single-token shadow** (`/setup` writes the legacy
   key via `SETUP_ENV_KEYS`, `config.rs:151`): in multi-bot mode the first-run
   wizard's "Bot token" is shadowed exactly like Settings' was. Out of scope
   here; the §3.2 mode line does not cover it.
3. **Legacy badge heuristic** keys "not connected" on `owner_id == 0`
   (`settings.rs:231-235`), not on token presence. Legacy-mode-only; ride
   along only if §4's byte-identical constraint is ever relaxed.
