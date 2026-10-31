# MANAGEMENT_DISCLOSURE_SPEC — progressive disclosure for the management surfaces

Status: **READY FOR L7** · Owner-mandated redesign (demonstrated 2026-10-31: form
walls, meaningless `#id` titles, empty states that lie).
Sources of truth: `DESIGN.md` + `assets/app.css`. Master: this file.
**Zero token changes** — every color/spacing value below reuses existing tokens
(`--line`, `--ink-3`, `--surface`…) → **nothing needs L6-C**.

Supersedes, for this wave only: `VISUAL_POLISH_SPEC.md` §B16 ("report-only")
and its F-3 budget flag — the owner explicitly ordered the `bots_admin.rs`
split; B16 F-1 is folded into Item 1 below. B16 F-2 rides as an optional rider.

House idioms being applied (do not invent new ones):
- One primary action per screen. The Bots page's primary action is **Add bot**;
  inside an expanded card it is **Save**; Disable/Delete stay secondary
  (`btn-sm btn-secondary` / `btn-sm btn-danger`).
- `<details>` accordions as shipped on Settings
  (`src/web/settings.rs:260,292,314,341` — four `<details open id=…><summary>…</summary>
  <article class="panel">…</article></details>`; rules at `assets/app.css:515-535`).
- `help` / `alert`(+`alert-info/success/warn/error`) / `badge`(+`-success/-warning/-danger`)
  / `stat` classes exactly as defined in `assets/app.css:336,464-482` and `DESIGN.md` §5.
- Banners: `role="status" aria-live="polite"` (success/info/warn) and
  `role="alert"` (error) — byte-identical through the §0 helper.
- WCAG 2.1 AA floor: `<details>/<summary>` is natively keyboard-operable
  (Enter/Space toggle, no JS, no `tabindex`); `:focus-visible`
  (`assets/app.css:116`) gives summaries the brand ring for free; `aria-current`
  lives in the nav (`src/web/html.rs:258`) and is not touched by anything here.

---

## §0 Audit evidence (read before write)

| Fact | Evidence |
|---|---|
| `bots_admin.rs` is 1,128 lines — over the 800 budget | `wc -l`; flag at `VISUAL_POLISH_SPEC.md:459-466,578` |
| Card head prints the DB row id as `#3` | `src/web/bots_admin.rs:286` `<h3>{key} <span class="mono">#{id}</span></h3>` |
| Disable/Delete are two stacked bare forms (B16 F-1) | `src/web/bots_admin.rs:302-307` |
| Token hint is a `.secret-hint` pill, sole user | `src/web/bots_admin.rs:288`; CSS `.secret-hint` `assets/app.css:564-570` (no other use in `src/`) |
| Settings accordion idiom to mirror | `src/web/settings.rs:260,292,314,341`; `details > article.panel` flat-attach `assets/app.css:530-535` |
| `.card` carries NO surface (only box-shadow/h3/p) — the surface lives on `article, .panel` | `assets/app.css:226-235` → cards must use **`panel`**, not `card` |
| `.bot-card` is `display:flex` — breaks a `<details>` (summary+body would sit side-by-side) | `assets/app.css:553`; `.bot-card-head` 554-559; `.bot-card-form` 560-563 |
| Lying scoped empty state: the `seller_id==0` rows are filtered to the selected bot in ONE pass, so a bot-scoped view shows "No global channels yet." while rows exist | `src/web/channels.rs:242-256`; verified today: with no bot tokens every channel resolves to placeholder bot id 0, so ANY non-zero bot filter is empty |
| Add-form help wall (owner/global form) | `src/web/channels.rs:260` (4-line run-on in one `span.help`) |
| Same wall on the seller form | `src/web/channels.rs:220` |
| Rename box pre-fills the channel **identifier** as the "new name", placeholder invites `@newchannel` | `src/web/channels.rs:361` (`value="{}"` ← `esc(&row.channel)`, `placeholder="@newchannel"`), hidden `name="channel"` at :361 |
| Rename success banner, `?renamed=1` | `src/web/channels.rs:66-72`; handler redirect `src/web/channels.rs:679-683` |
| `?notitle=1` pattern to mirror (handler-side flag → honest info banner instead of success) | `src/web/channels.rs:36-53` (banners), `:489-496` (handler) |
| Channels NOCASE collation (justifies `eq_ignore_ascii_case`) | doc at `src/web/channels.rs:434-438` |
| `bots.toml` writer already extracted — DO NOT TOUCH | `src/web/bots_config.rs` (225 lines) |
| Glob wiring the split relies on | `src/web/mod.rs:73-74` (`use bots_admin::*; use bots_config::*;`), routes `:382-388` |
| Shared token-validation users resolve UNQUALIFIED through the glob | `src/web/channel_ops.rs:451-452`, `src/web/settings.rs:468-469` → zero importer churn when `use bots_add::*;` is added |
| `referer_path` users (STAYS in bots_admin.rs) | `src/web/identity.rs:623`, `src/web/smoke_tests.rs:603-629` |
| `focused_error_alert` / `SECRET_TOGGLE` / `esc` / `js_arg` / `urlencode_query` reachable from any sibling via `use super::*;` | `src/web/identity.rs:241`, `src/web/mod.rs:446,179`, `src/web/html.rs:104,108` |

At-risk test assertions (full ledger in §6): the only **must-update** pin in
`src/web/tests/bots.rs` is the `?saved=1` location assert (:145-149); everything
else verified safe. `src/web/tests/channels.rs` pins none of the touched copy.
`src/web/mfa_users.rs` renders its own `.panel` surfaces (MFA/setup) — it is
**not** a bot-card consumer; nothing there changes and the Settings
`details > article.panel` rule (`app.css:530-535`) must remain untouched.

---

## §1 SPLIT FIRST — `src/web/bots_admin.rs` → new sibling `src/web/bots_add.rs`

**Move-only.** No signature, logic, copy, or markup edits in this step. Review
the diff as a rename (`git diff --find-renames`); the Item 1–4 behavior edits
ride only after the move is proven.

### 1a. What moves to `src/web/bots_add.rs`

New file header:

```rust
use super::*;
// ---------- Bot add wizard + bot-token validation (moved verbatim from
// bots_admin.rs, MANAGEMENT_DISCLOSURE_SPEC §1; no logic changes) ----------
use super::bots_admin::BotsQuery; // shared page query: /bots and /bots/add
```

(`BotsQuery` stays in `bots_admin.rs` — `bots_page` is its primary consumer;
its `pub(super)` struct + fields are visible to the sibling. Everything else
the cluster needs — `esc`, `html_page`, `focused_error_alert`, `WebState`,
`Session`, `bot_config_state`, `bot_toml_block`, `append_bot_block`,
`duplicate_key_in`, `parse_toml_document`, `ConfigWriteResult`, `sha256_hex` —
resolves via `use super::*;` exactly as it does in `channels.rs` et al.)

| # | Item | Current lines in `bots_admin.rs` |
|---|---|---|
| 1 | `BotValidateForm` | 36-39 |
| 2 | `BotAddForm` | 41-50 |
| 3 | Wizard section banner comment + `LAST_BOT_SNIPPET` static | 397-399 |
| 4 | `is_slug_key` | 401-406 |
| 5 | `is_token_env_name` | 408-416 |
| 6 | `default_channel_value` | 418-424 |
| 7 | `validate_default_channel` | 426-435 |
| 8 | `TokenValidationError` (+doc) | 437-443 |
| 9 | `token_error_copy` (+doc) | 445-453 |
| 10 | `validate_token_username` (+doc) | 455-506 |
| 11 | `duplicate_key_msg` (+doc) | 508-515 |
| 12 | `BotAddPreserved` (+impl) | 517-567 |
| 13 | `suggested_token_env` | 569-572 |
| 14 | `BotAddValidated` + `validate_bot_add` | 574-634 |
| 15 | `bot_add_error_page` (+doc) | 636-648 |
| 16 | `bot_snippet_panel` (+doc) | 650-664 |
| 17 | `render_add_page` (+doc) | 666-737 |
| 18 | `bots_add_page` | 739-771 |
| 19 | `bots_add_validate` | 773-832 |
| 20 | `bots_add_submit` | 834-960 |
| 21 | `bots_add_snippet` | 962-995 |
| 22 | Tests `validated_username_prefills_step2` (1101-1112) + `suggested_token_env_folds_separators` (1114-1118) with their doc comments | 1101-1118 (into a new `#[cfg(test)] mod tests { use super::*; … }`) |

Items 8-10 move so the wizard block stays contiguous; they are the shared
token-validation library. Their users (`channel_ops.rs:451`, `settings.rs:468`)
reference them **unqualified**, so step 1b keeps them compiling with zero edits.

### 1b. Wiring (the only non-move edits allowed in this step)

`src/web/mod.rs` — two lines, alphabetical, next to the existing pair:

```rust
mod bots_add;      // before `mod bots_admin;` (:30)
use bots_add::*;   // before `use bots_admin::*;` (:73)
```

Routes (`mod.rs:382-388`) change **nothing** — `bots_add_page`,
`bots_add_submit`, `bots_add_validate`, `bots_add_snippet` resolve through the
new glob exactly as they did through `use bots_admin::*;`. No new routes, no
route renames, no `bots_config.rs` edits.

### 1c. Ledger (proof required before the behavior edits)

- Moved ≈ 633 lines (15 + 599 + 19). `bots_admin.rs` lands ≈ **495** lines
  (keeps: header 1-8 + `BotSelectForm`/`BotSettingsForm`/`BotsQuery` 10-34 +
  `session_token_from_headers` 55-63 + `referer_path` 74-91 + `bot_select`
  93-123 + `bot_key_url_param` 125-139 + `banner_bot_key` 141-152 +
  `bots_page_banners` 154-186 + `bots_page_html` 188-233 + `bot_card_html`
  235-322 + `bots_page` 324-340 + `bots_save` 342-380 + `bots_save_error_page`
  382-395 + `bots_disable` 997-1039 + `bots_delete` 1041-1095 + test
  `bot_key_url_param_guards_url_safety` 1120-1127). `bots_add.rs` ≈ 640.
- Gate: `wc -l src/web/bots_admin.rs` ≤ **800** and `wc -l src/web/bots_add.rs`
  ≤ **800** BEFORE any Item 1-4 edit; `cargo build && cargo test -q` green on
  the pure move (byte-identical pages — assert with the existing suite).
- Grandfathered: `bots_add_submit`'s body is 124 lines (pre-existing). It moves
  verbatim; if the behavior pass touches it, extract the success-response block
  (`:923-934`) into `fn bot_add_success_response(...) -> Response` to get under
  the 100-line body rule. All functions this spec ADDS are ≤ 100-line bodies.
- Add a one-line pointer comment at the old wizard site in `bots_admin.rs`:
  `// Add wizard + token validation live in bots_add.rs (MANAGEMENT_DISCLOSURE_SPEC §1).`

---

## §2 ITEM 1 — Bots cards collapse (progressive disclosure)

File: `src/web/bots_admin.rs` — `bot_card_html` (:237-322 pre-split) and
`bots_page_html` (:188-233 pre-split). One `<details>` per bot, **collapsed by
default**, mirroring the Settings accordion (`settings.rs:260`) but with the
card itself as the surface — hence `panel`, not the coordinator shorthand
`card` (`.card` has no surface rules, `assets/app.css:226-235`).

### 2a. Markup (exact replacement for the `format!` in `bot_card_html`)

```html
<details class="panel bot-card" id="bot-{id}"{open_attr}>
  <summary>
    <h3>{key}</h3>
    {enabled_badge}
    {token_badge}
    <span class="bot-card-owner">owner {owner_val}</span>
    <span class="bot-card-hint">Expand to edit owner, default channel, or enabled state</span>
  </summary>
  <div class="bot-card-body">
    <form method="post" action="{bots_url}" class="bot-card-form">
      <input type="hidden" name="bot_id" value="{id}">
      <label for="bot-owner-{id}">Owner user ID</label>
      <input id="bot-owner-{id}" type="number" name="owner_user_id" value="{owner_val}" required>
      <span class="help">Your own Telegram account number (@userinfobot replies with it).</span>
      <label for="bot-channel-{id}">Default channel ID</label>
      <input id="bot-channel-{id}" type="number" name="default_channel_id" value="{channel_val}">
      <span class="help">Numeric chat ID of the channel (starts with -100…). Leave blank if this bot has no default channel.</span>
      <label class="check"><input type="checkbox" name="enabled"{enabled_attr}> Enabled</label>
      <button type="submit">Save</button>
    </form>
    <p class="help">Disabling is reversible — the bot stays listed, it just stops after the next restart. Deleting removes its settings from the console.</p>
    <div class="row-actions">
      <form method="post" action="{disable_url}" class="inline-form">
        <button type="submit" class="btn btn-sm btn-secondary" onclick="return confirm('Disable {js_key}? It stops responding after the next restart. You can enable it again any time.')">Disable</button>
      </form>
      <form method="post" action="{delete_url}" class="inline-form">
        <button type="submit" class="btn btn-sm btn-danger" onclick="return confirm('Delete {js_key}? Its settings are removed from the console. Bots that come from your config file will reappear on restart unless you remove them from the file.')">Delete</button>
      </form>
    </div>
    <p class="help">Tokens are stored only in your config file (.env / bots.toml) and are read once at startup — this console never shows or edits a token. Owner and channel changes here apply on restart.</p>
  </div>
</details>
```

Exact deltas:
- **Kill `#{id}`**: the `<h3>{key} <span class="mono">#{id}</span></h3>` at
  :286 becomes `<h3>{key}</h3>`. The DB row id is meaningless to users; the
  `id="bot-{id}"` anchor attribute stays (deep-link target), it is just no
  longer displayed.
- **Enabled badge** unchanged (:259-263): `badge-success` "enabled" /
  `badge-warning` "disabled".
- **Token-state badge** replaces the `.secret-hint` pill (:288) — same three
  states, same derived value, badge classes instead:

```rust
let token_badge = match state.bots.get(b.id) {
    Some(ctx) if !ctx.bot.token().trim().is_empty() => {
        let last4: String = /* identical to today's rev/take(4) derivation */;
        format!(r#"<span class="badge">token …{}</span>"#, esc(&last4))
    }
    Some(_) => r#"<span class="badge badge-warning">token not saved yet</span>"#.to_string(),
    None => r#"<span class="badge">token in config file</span>"#.to_string(),
};
```
- **Owner id as muted text**: `<span class="bot-card-owner">owner {owner_val}</span>`
  (`owner_val` = `b.owner_user_id.to_string()`, `esc()`ed as today).
- **Summary-action hint** (one line, right-aligned): the `bot-card-hint` span.
- **B16 F-1 folded in**: Disable/Delete now sit inside one
  `<div class="row-actions">` (matches every list page, e.g. `channels.rs:378-381`).
- `js_key`, confirm copy, form fields, help copy: byte-identical to today.

### 2b. Open-the-saved-card

- `BotsQuery` (bots_admin.rs:23-34) gains one additive field:
  `pub(super) bot: Option<String>,`
- `bots_save` (:379 pre-split) redirects to
  `"/bots?saved=1&bot={}"` with the saved row id — banner logic in
  `bots_page_banners` (:156-186) is unchanged (`q.saved` non-empty check).
- `bots_page_html` computes `let open_bot = q.bot.as_deref().and_then(|s| s.trim().parse::<i64>().ok());`
  and `bot_card_html` takes `open: bool` → `{open_attr}` = `" open"` or `""`.
  `bots_save_error_page` passes `BotsQuery::default()` → nothing opens. No
  other query shape changes; `/bots/add` (which parses `BotsQuery` too) is inert.

### 2c. CSS (exact replacement inside the `bot cards` block, `assets/app.css:551-570`)

Replace `.bot-card` (:553), `.bot-card-head` (:554-559) with:

```css
/* Collapsed accordion cards (MANAGEMENT_DISCLOSURE_SPEC §2): the summary is
   the card header; form + actions live in the expanded body. */
details.bot-card { display: block; padding: 0; } /* .panel surface, padding moves to summary/body */
details.bot-card > summary {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  padding: 14px 18px;
}
details.bot-card > summary h3 { margin: 0; font-size: 14px; font-weight: 600; }
.bot-card-owner, .bot-card-hint { color: var(--ink-3); font-size: 12px; }
.bot-card-hint { margin-left: auto; }
.bot-card-body {
  display: flex; flex-direction: column; gap: 12px;
  padding: 14px 18px 18px;
  border-top: 1px solid var(--line);
}
```

Keep `.bot-card-form` rules (:560-563) verbatim. Delete `.secret-hint`
(:564-570) — its only consumer is gone (verified: `grep -rn secret-hint src/`
→ `bots_admin.rs:288` only). `details > article.panel` (:530-535) is untouched
— bot-card bodies are `div`, not `article.panel`, so Settings is unaffected.
The global `details summary` rules (:515-524) still supply the ▸/▾ marker and
`cursor: pointer`; `:focus-visible` (:116) covers keyboard focus. Zero tokens.

---

## §3 ITEM 2 — honest scoped empty state (Channels)

File: `src/web/channels.rs`, `render_global_channels` (:230-279). The rows
chain (:242-250) filters to `seller_id == 0` and to the selected bot in one
pass, then :251-256 renders "No global channels yet." even when the emptiness
is purely a scoping artifact. The function needs the **unfiltered**
`seller_id == 0` count; one `list_channels` fetch serves both (no new SQL, no
DB change — the second pass is over the already-fetched `Vec`).

Exact replacement for :242-256:

```rust
    let fetched = state.db.list_channels(None);
    // An empty SCOPED list with rows present is a scoping artifact, not an
    // empty set (MANAGEMENT_DISCLOSURE_SPEC §3) — count the unscoped rows.
    let total_global = fetched.iter().filter(|row| row.seller_id == 0).count();
    let rows = fetched
        .into_iter()
        .filter(|row| row.seller_id == 0)
        .filter(|row| scope_bot.is_none_or(|bid| state.bots.for_channel(&row.channel).id == bid))
        .map(|row| render_global_channel_row(state, is_owner, row))
        .collect::<Vec<_>>()
        .join("");
    let list = if rows.is_empty() && scope_bot.is_some() && total_global > 0 {
        "<li class=\"empty\"><b>No channels resolve to this bot yet.</b> The bot may not be connected (no token) or these channels belong to another bot — switch the Bot selector above to All bots.</li>".to_string()
    } else if rows.is_empty() {
        "<li class=\"empty\"><b>No global channels yet.</b> Add one below — it becomes a posting target for plans and posts.</li>".to_string()
    } else {
        rows
    };
```

- The genuine-empty copy (unscoped view, or scoped view with zero global rows)
  is preserved verbatim as the `else if` arm — it never lies.
- `scope_bot.is_some()` only happens for an owner with a bot selection
  (:236-241); admins/sellers keep the original copy.
- Net +4 lines. The scoped copy is prescribed verbatim — do not reword
  ("All bots" is the selector's option label, `tests/bots.rs:448`).

---

## §4 ITEM 3 — add-form density (Channels)

File: `src/web/channels.rs`. The 4-line run-on `span.help` walls (:220 seller
form, :260 owner/global form) collapse behind a `<details class="help-more">`,
reusing each form's **full existing text verbatim** inside. The visible line is
one sentence. Both forms keep their distinct final sentence inside the details.

Visible line (both forms, exact):
`@handle, t.me link, or -100 ID — the bot must be admin in the channel.`

Owner/global form — replace the `<span class="help">…</span>` inside the
`<form>` at :259-263 with (single source line, keeps the file budget neutral):

```html
<span class="help">@handle, t.me link, or -100 ID — the bot must be admin in the channel.</span><details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Accepted: @handle, t.me link, or numeric -100 ID. Recommended: the channel's @handle (memorable) or its t.me link. Private channel without a public @handle: paste the numeric chat ID (starts with -100) — forward any message from it to @userinfobot to get it. The bot must be an admin in the channel. Adding one does not change the default — use Set default.</span></details>
```

Seller form — same swap at :220, details content verbatim ending
`… The bot must be an admin in the channel. Adding one replaces the previous.`
(its own tail, unchanged).

CSS (new, non-token; add next to the `.help` rule, `assets/app.css:336`):

```css
/* Collapsed long-form help (MANAGEMENT_DISCLOSURE_SPEC §4) */
.help-more { margin: 2px 0 0; }
.help-more summary { padding: 2px 0; font-size: 12px; font-weight: 500; color: var(--ink-3); }
.help-more[open] summary { margin-bottom: 4px; }
```

The `details`/`summary` global rules (:515-524) supply the ▸/▾ affordance.
Net 0 lines (one source line replaced by one source line per form).

---

## §5 ITEM 4 — rename box tells the truth (Channels)

File: `src/web/channels.rs`. Two defects: the box pre-fills the channel
**identifier** as the "new name" (:361/:367, with `placeholder="@newchannel"`),
so a no-op submit "renames" the channel to its own ID under a success banner
(:66-72). **Approach: the param mirror of `?notitle=1`** (handler-side flag →
redirect → honest banner). Chosen over compute-at-render because
`channels_banners` (:32-126) would need a DB read it doesn't do, and matching
the submitted name against "any existing channel identifier" is ambiguous when
a second row already owns that identifier. The param is unambiguous, additive,
and matches the house PRG idiom (`ChannelsQuery` doc :128-134).

### 5a. Query field (+1 line)

`ChannelsQuery` (:135-148) gains: `pub(super) same: Option<String>,`

### 5b. Handler guard — `channels_rename` (:628-687), inserted after the
empty check (:672-674), BEFORE the DB call:

```rust
    // Self-rename guard (MANAGEMENT_DISCLOSURE_SPEC §5): a NOCASE match of
    // the box against the identifier changed nothing — say so (mirrors
    // ?notitle=1) instead of a false "renamed" banner.
    if old.eq_ignore_ascii_case(&new) {
        return Redirect::to(&state.url("/channels?renamed=1&same=1")).into_response();
    }
```

`eq_ignore_ascii_case` mirrors the NOCASE column collation (`channels.rs:437`).
Nothing is written; the stored title survives.

### 5c. Honest banner — `channels_banners`, the `renamed` branch (:66-72) becomes:

```rust
    if q.renamed.as_deref() == Some("1") && q.same.as_deref() == Some("1") {
        banners.push(channels_banner("info", "Nothing changed — that's still the channel's ID. Clear the box and type a name to label it."));
    } else if q.renamed.as_deref() == Some("1") {
        let name = q.channel.as_deref().unwrap_or("the channel");
        banners.push(channels_banner("success", &format!("Channel renamed to {}.", esc(name))));
    }
```

### 5d. Pre-fill from the title — `render_global_channel_row`'s `rename_details`
(:358-372). The hidden `name="channel"` input stays (it carries the
identifier); the visible box gets the human title, else EMPTY + placeholder:

```rust
    let rename_id = esc(&format!("rename-{}", row.channel));
    // MANAGEMENT_DISCLOSURE_SPEC §5: pre-fill the stored title when one
    // exists; else EMPTY with a plain placeholder — the identifier rides the
    // hidden `channel` field, never the visible box.
    let current = row.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let (rename_value, rename_ph) = (
        current.map(esc).unwrap_or_default(),
        if current.is_some() { "" } else { "Friendly name, e.g. Tech Tips" },
    );
```

and the input inside the form string (:361) becomes
`<input id="{}" name="new" value="{}" placeholder="{rename_ph}" required>`
(placeholder attr always rendered; `placeholder=""` is inert when a title
exists, visible exactly when the box is empty — no `@` anywhere).

### 5e. §0 budget compensator (behavior-preserving, REQUIRED — pays for §3/§5)

The current file is **793** lines and the cap is 800; §3+§5 add ≈ +13 raw. To
keep the hard cap honest, deduplicate the banner wrapper in `channels_banners`
into one helper (byte-identical output, provable by the existing suite):

```rust
/// One console banner line (MANAGEMENT_DISCLOSURE_SPEC §5e): fixed wrapper,
/// caller-owned copy. info/success/warn announce politely; error is
/// assertive — output is byte-identical to the previous inline blocks.
fn channels_banner(kind: &str, msg: &str) -> String {
    if kind == "error" {
        format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, msg)
    } else {
        format!(
            r#"<div class="alert alert-{kind}" role="status" aria-live="polite">{}</div>"#,
            msg
        )
    }
}
```

Swap the fixed-copy pushes (:48-52 added, :54-58 default, :60-64 removed,
:76-82 bound-user, :83-89 bound-bot, :120-123 error; the notitle :44-46 and
renamed branches per §5c) to `banners.push(channels_banner("info"/"success"/"warn", …))`.
Net ≈ **−20**; projected file ≈ **786** lines. Gate: `wc -l src/web/channels.rs`
≤ **800** after §3-§5. (`channels_body`'s inline alert wrapper at :162-164 MAY
also call the helper — optional, same bytes.)

---

## §6 Constraints & gates for L7

1. **Order**: §1 move-only → `wc -l` proof (both files ≤ 800) + `cargo build &&
   cargo test -q && cargo clippy -- -D warnings && cargo fmt --check` green →
   then §2, then §3-§5 (channels), each with tests. No logic edits inside the
   move; no markup edits before the move is merged/committed separately.
2. **No new routes** (mod.rs gains exactly `mod bots_add;` + `use bots_add::*;`;
   route table :382-388 byte-identical). **No DB changes** (no schema, no new
   SQL; §3 reuses `list_channels(None)`; `db.rs` untouched). **`bots_config.rs`
   untouched.**
3. **`esc()` everywhere** user-derived data lands in markup (keys, token last4,
   rename value, banner names) — as today; `js_arg` for confirm() strings.
4. **Function bodies ≤ 100** for anything added/edited (§1c grandfather note for
   the moved `bots_add_submit`). `render_global_channels` stays ≤ ~50 after §3;
   `channels_rename` ≤ ~60 after §5b.
5. **channels.rs hard cap 800** (currently 793): §3-§5 land only with the §5e
   compensator; re-run `wc -l` after each item.
6. **a11y floor**: `details/summary` native (no JS, no positive `tabindex`);
   `aria-current` (html.rs:258) untouched; `:focus-visible` ring preserved (do
   not add `outline: none`); sr-only rename label kept; banner `role`/`aria-live`
   attributes byte-identical through `channels_banner`; `label class="check"`
   and `for=`/`id=` pairs preserved in the card body.
7. **One primary action per screen** is preserved: page = Add bot; card = Save;
   Disable/Delete stay small/secondary inside the body.

### At-risk test ledger

**MUST update (behavior change is the point):**
- `src/web/tests/bots.rs:145-149` — `assert_eq!(location, "/bots?saved=1")`
  becomes `"/bots?saved=1&bot={bot_id}"` (same `format!` style; :169-177's
  `/bots?saved=1` GET still shows the banner — `q.saved` check unchanged).

**Verified SAFE (pinned strings that survive; do not weaken them):**
- `tests/bots.rs:115` `"secondary"`, :116 `"multi-bot management"` (h1 sub kept);
  :174-177 `"Bot settings saved — they apply on restart."`; :199/:213
  `"Nothing was changed."` + `data-focus-on-load`; :227-232 add-wizard step
  lines + absence of `name="token_env"` / `name="owner_user_id"` (wizard markup
  unchanged by the split); :254-261 preserved-typed-values; :291-295/:322-326
  `/bots/add?added=…` redirects; :372-374, :397-401 disable/delete banners;
  :420-424 honest no-op banners; :446-461 shell bot-scope assertions.
- `tests/channels.rs` pins none of the touched copy (its asserts: `@one`/`@two`,
  `"default"`, `"cannot be removed"`, `"bot-managed"`, `"not yours to remove"`,
  `<select name="channel">`) — all survive §3-§5.
- No test pins `#`+id, `bot-card-head`, `.secret-hint`, the old empty-state
  copy, the old help wall, `@newchannel`, or `"Channel renamed to"` (grepped
  `src/web/tests/` + `src/web/*_tests.rs`). `mfa_users.rs` / Settings panels:
  untouched consumers — keep it that way.

**Prefer additive new tests (suggest, in `tests/bots.rs` / `tests/channels.rs`):**
- Bots page: body contains `<details class="panel bot-card"` and
  `Expand to edit owner`, does NOT contain `<span class="mono">#`;
  collapsed by default (no `bot-card" id="bot-N" open>` without `?saved`);
  with `?saved=1&bot=N` exactly that card carries ` open>`.
- Channels: owner with a bot selection + ≥1 global row not resolving to it →
  contains `"No channels resolve to this bot yet."` and not
  `"No global channels yet."`; unscoped empty view still shows the original copy.
- Channels: GET contains the one-liner `@handle, t.me link, or -100 ID — the
  bot must be admin in the channel.` and
  `<details class="help-more"><summary>Which forms are accepted?</summary>`.
- Rename: POST `new == identifier` → redirected `/channels?renamed=1&same=1`,
  GET shows the alert-info copy and NOT `"Channel renamed to"`; form with a
  stored title pre-fills `value="{title}"`; without one renders `value=""` +
  `placeholder="Friendly name, e.g. Tech Tips"`.

### Optional rider (B16 F-2, one line, while in `bots_add.rs`)

`render_add_page`'s token input (:681 pre-split) is the console's only secret
field without the reveal toggle: wrap it as
`<span class="secret-field"><input id="bot-add-token" type="password" name="token" required autocomplete="new-password" placeholder="123456:ABC-DEF…">{toggle}</span>`
with `toggle = SECRET_TOGGLE` (`mod.rs:446`) — same structure as
`settings.rs:265`. Only if it does not perturb the §1 diff review; otherwise
defer.
