# Channel Title UX — Titleless Rows, Honest Add, One Label Everywhere

**Status:** Approved (3-item fix), ready for web implementation
**Date:** 2026-10-31
**Discipline:** design
**Territory of this spec:** console display of channel identifiers only. No
new routes, no schema, no bot-side changes.
**Sources of truth:** `DESIGN.md` + `assets/app.css` (MASTER.md is
deprecated 2026-10-31 and must not be used). Related spec:
`IDENTITY_SENDER_UX_SPEC.md` §2/§4 (badge placement this spec must not
disturb).

---

## 0. Summary of decisions

1. **Titleless row hint** — a Channels-page row with no stored title renders
   the identifier PLUS one muted inline hint (`span.help`). Both row builders
   get it: owner (`render_global_channel_row`) and seller
   (`render_seller_channels`). Copy is role-honest: the owner is pointed at
   the Rename button; the seller gets the plain fact (the seller view has no
   Rename button — see §2.4).
2. **Honest add-when-unresolved** — when the add flow's `channel_chat_info`
   best-effort match yields `None`, the success redirect carries
   `?added=1&notitle=1` and the page shows ONE `alert alert-info` banner that
   says the name could not be fetched, instead of the plain "Channel added."
   success. Exact param, banner, and branch shapes in §3.
3. **One label everywhere** — a shared `channel_label(title, channel)`
   helper renders `"Title (-100…)"` when a title is stored, the bare
   identifier otherwise, at every console site that displays a channel
   identifier (dashboard stat + composer, Posts, Subs, Sellers admin, Monitor
   picker). The "no title" hint renders ONLY on the Channels page, where the
   Rename action lives. Full audit with file:line in §4.

Non-goals: no "Fetch title" button, no new routes, no re-resolution of
stored titles, no changes to form values / hidden inputs / confirm() strings /
aria-labels (they must keep quoting the raw identifier).

---

## 1. Verified current state

- Owner row builder: `src/web/channels.rs:239-354`
  (`render_global_channel_row`). Label logic at `channels.rs:340-345`:
  `Some(t) => format!("{} ({})", esc(t), esc(&row.channel))`,
  `None => esc(&row.channel)` — the None arm renders a bare identifier.
- Seller row builder: `src/web/channels.rs:148-186`
  (`render_seller_channels`). Its row (`channels.rs:161-169`) always renders
  `esc(&ch)` — it has NO title data today: it reads
  `Db::seller_channel(sid)` (`src/db.rs:841`), which returns the bare string
  from `sellers.default_channel`. A title lookup must go through the existing
  `Db::list_channels(Some(sid))` read (`src/db.rs:5745`), whose `ChannelRow`
  (`src/db.rs:86-93`) carries `title: Option<String>`.
- Titles are written in exactly one place: the owner branch of the add flow
  (`channels.rs:427` → `Db::set_channel_title`, `src/db.rs:1998`, a no-op
  without an existing `channels` row). Titles are never written for sellers
  and never re-fetched later — this is why titleless rows are permanent
  without Rename.
- Rename semantics (important for copy): `POST /channels/rename` →
  `Db::rename_channel` (`src/db.rs:5617`) rewrites the **stored identifier**
  (`UPDATE channels SET channel = …`); it does NOT set the Telegram-side
  title and does not call `set_chat_title` (that is the separate
  `/channels/title` bot-API route, `src/web/channel_ops.rs:264`, out of
  scope). "Use Rename to name it" is therefore correct user guidance for a
  bare `-100…` row: renaming the identifier to the channel's `@handle` is
  exactly how the row gains a readable name.
- Add flow: `channels.rs:382-435` (`channels_add`). Best-effort resolution at
  `channels.rs:410-413`; seller success redirect at `:420`, owner success
  redirect at `:428` — both currently `?added=1` unconditionally.
- Banner pattern to mirror: `?renamed=1` at `channels.rs:34-40`
  (`alert alert-success`, `role="status"`, `aria-live="polite"`); the
  role-branched copy precedent is `error=posting`/`error=identity` at
  `channels.rs:63-87`.
- House classes (all exist, no CSS changes): `.help`
  (`assets/app.css:336` — 12px, `--ink-3`, muted), `.alert-info`
  (`assets/app.css:499`), `.badge`/`.badge-success`/`.badge-warning`
  (`assets/app.css:464-482`), `.mono` (`assets/app.css:428`). Channels-page
  `li` rows are `display:flex; gap:10px; flex-wrap:wrap`
  (`assets/app.css:406-415`) — the hint renders as a flex item and wraps
  cleanly on mobile with zero CSS work.
- Visibility plumbing for the new helper: `src/web/mod.rs:77` does
  `use channels::*;` and every web module does `use super::*;`, so a
  `pub(super) fn` added to `channels.rs` is callable from `dashboard.rs`,
  `content.rs`, `subs.rs`, `admin_console.rs`, `monitor.rs` with no new
  imports.

---

## 2. Item 1 — Titleless row hint (Channels page, both views)

### 2.1 Owner row (`render_global_channel_row`, channels.rs:239-354)

Replace the label block at `channels.rs:340-345` and extend the row format at
`channels.rs:346-353`. Exact shape:

```rust
// Show the human title (when known) alongside the canonical id so a title
// and its id are never mistaken for two different channels (CHANNELS_TITLE_UX_SPEC §2).
let label = channel_label(row.title.as_deref(), &row.channel);
let notitle_hint = if row.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
    r#"<span class="help">no title yet — use Rename to name it</span>"#
} else {
    ""
};
format!(
    "<li><span class=\"mono\">{}</span> {}{badge}{user_badge}{notitle_hint}<div class=\"row-actions\">{actions}</div></li>",
    label,
    type_badge,
    badge = badge,
    user_badge = user_badge,
    notitle_hint = notitle_hint,
    actions = actions
)
```

Rendered result for a titleless row:

```html
<li><span class="mono">-1001234567890</span> <span class="badge">private</span><span class="badge badge-success">default</span><span class="help">no title yet — use Rename to name it</span><div class="row-actions">…</div></li>
```

Placement rule: the hint is the LAST element of the label cluster — after
`{type_badge}{badge}{user_badge}`, before `<div class="row-actions">`. Do NOT
insert it between the identifier and the badges: `IDENTITY_SENDER_UX_SPEC`
§2 places the `as user` badge at the channel name, and the existing
adjacency must survive untouched.

Copy (verbatim, em dash as literal UTF-8, matching existing banner prose):

```
no title yet — use Rename to name it
```

The hint is static text — no `esc()` needed. `label` is fully escaped inside
`channel_label`. The hint is visible text (not aria-only), so it reaches
screen readers naturally; add no `aria-label`.

### 2.2 Seller row (`render_seller_channels`, channels.rs:148-186)

The seller view must show the same treatment. It has no title in hand, so
look it up through the existing `list_channels(Some(sid))` read (no DB
change). Exact shape inside the `Some(ch)` arm (bind `sid` first — reuse the
value already computed at `channels.rs:154`):

```rust
let sid = session.seller_id.unwrap_or(0);
let list = match state.db.seller_channel(sid) {
    Some(ch) => {
        // Same label contract as the owner rows (CHANNELS_TITLE_UX_SPEC §2):
        // the seller's channel lives in sellers.default_channel (no title
        // column), so its title — when the same channel is also registered
        // globally — is read from the channels table, case-insensitively.
        let title = state
            .db
            .list_channels(Some(sid))
            .into_iter()
            .find(|row| row.channel.eq_ignore_ascii_case(&ch))
            .and_then(|row| row.title);
        let label = channel_label(title.as_deref(), &ch);
        let notitle_hint = if title.as_deref().is_none_or(|t| t.trim().is_empty()) {
            r#"<span class="help">no title stored for this channel yet</span>"#
        } else {
            ""
        };
        let type_badge = channel_type_badge(&ch);
        let confirm_msg = esc(&format!("Remove {}? This cannot be undone.", js_arg(&ch)));
        format!(
            "<li><span class=\"mono\">{}</span> {}<span class=\"badge badge-success\">bot-managed</span>{} <div class=\"row-actions\"><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\" aria-label=\"Remove {}\" onclick=\"return confirm('{}')\">Delete</button></form></div></li>",
            label,
            type_badge,
            notitle_hint,
            state.url("/channels/remove"),
            esc(&ch),
            esc(&ch),
            confirm_msg
        )
    }
    None => /* unchanged empty-state li */,
};
```

Seller hint copy (verbatim):

```
no title stored for this channel yet
```

**Why the seller copy differs from the owner copy:** sellers get no Rename
button in this view (`render_seller_channels` renders Delete only) and the
rename path cannot serve them — `Db::rename_channel(sid, …)` updates
`channels WHERE seller_id = sid` (`src/db.rs:5631-5634`), and seller channels
are stored in `sellers.default_channel`, not as `channels` rows, so a seller
Rename would fail with "channel not found (or not yours to rename)". Pointing
a seller at a dead action violates the honesty bar; the seller hint states
the fact and stops. (Owner-side remedy is a §8 follow-up, not this pass.)

**Uniformity note for web:** the hint fires on EVERY titleless row, including
rows whose identifier is an `@handle`. That is intentional — the condition is
"no title", not "looks like a bare ID" (one rule, no heuristics). On an
`@handle` row the hint is muted 12px `--ink-3` and reads as "the Telegram
name isn't known", which is true. Do not add a numeric-ID conditional.

### 2.3 Both views, one contract

| View | Builder | Title source | Titleless render |
|---|---|---|---|
| Owner | `render_global_channel_row` (channels.rs:239) | `ChannelRow.title` in hand | `-100…` + `no title yet — use Rename to name it` |
| Seller | `render_seller_channels` (channels.rs:148) | `list_channels(Some(sid))` NOCASE match | identifier + `no title stored for this channel yet` |

### 2.4 Rename semantics (context for reviewers, no code)

Rename rewrites the stored identifier (§1). After renaming `-100…` →
`@mychannel`, the row shows `@mychannel` with the `public` badge; the stored
`title` stays absent and the hint remains — correct, because the Telegram
title genuinely is unknown. The row is no longer a bare numeric ID, which is
the state item 1 exists to end.

---

## 3. Item 2 — Honest add-when-unresolved (`?notitle=1`)

### 3.1 Param

`ChannelsQuery` (`channels.rs:112-124`) gains one field, listed with the
other success flags:

```rust
pub(super) notitle: Option<String>,
```

Extend the struct doc comment (`channels.rs:107-111`) and the banner-list
comment (`channels.rs:12-14`) to include `?notitle=1` — "add succeeded but
the Telegram title could not be resolved".

### 3.2 Banner (verbatim)

In `channels_page`, the `added` arm (`channels.rs:16-21`) becomes a branch:
`notitle=1` REPLACES the plain success banner (one outcome, one banner —
showing both would repeat "Channel added" twice). The role branch mirrors the
in-file `error=posting` precedent (`channels.rs:66-73`):

```rust
if q.added.as_deref() == Some("1") {
    if q.notitle.as_deref() == Some("1") {
        // Honest add (CHANNELS_TITLE_UX_SPEC §3): resolution failed, say so.
        let tail = if session.role == "owner" {
            " Use Rename to name it."
        } else {
            ""
        };
        banners.push(format!(
            r#"<div class="alert alert-info" role="status" aria-live="polite">Channel added — I couldn't fetch its name from Telegram (the bot may not be an admin there yet).{tail}</div>"#
        ));
    } else {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Channel added.</div>"#.to_string(),
        );
    }
}
```

Verbatim strings:

- Owner/admin: `Channel added — I couldn't fetch its name from Telegram (the bot may not be an admin there yet). Use Rename to name it.`
- Seller (no Rename button exists for sellers — §2.4): `Channel added — I couldn't fetch its name from Telegram (the bot may not be an admin there yet).`

`alert alert-info` (not `alert-success`): the outcome is mixed — the add
succeeded, the name is missing — and `--brand`-edged info is the honest
register (DESIGN.md: calm, never loud). `role="status" aria-live="polite"`
matches every other banner on this page.

### 3.3 Add-handler branch

The flag rides the `None` arm of the best-effort match: `channel_chat_info`
returning `None` sets `title = None` (`channels.rs:412`), and both success
redirects branch on it. Exact shape at `channels.rs:410-413`:

```rust
let (canonical, title) = match channel_chat_info(&bot.bot, ch).await {
    Some((id, title)) => (id.to_string(), title),
    None => (ch.to_string(), None),
};
// PRG honesty (CHANNELS_TITLE_UX_SPEC §3): when getChat couldn't answer,
// the redirect carries ?notitle=1 so the page says so instead of a bare
// "Channel added."
let added_q = if title.is_none() {
    "/channels?added=1&notitle=1"
} else {
    "/channels?added=1"
};
```

Then the seller success redirect (`channels.rs:420`) and the owner success
redirect (`channels.rs:428`) both become:

```rust
Redirect::to(&state.url(added_q)).into_response()
```

`state.url(&format!(…))` is NOT needed — `added_q` is a fixed literal, so
base-path prefixing takes it unchanged. All other arms of `channels_add`
(error, forbidden, empty) are untouched. `channels_add` stays ≈57 lines
(budget 100).

Intentional repetition: on `?added=1&notitle=1` the owner sees the banner AND
the new row's §2 hint with the same "use Rename" pointer. The banner is
transient (dies with the URL param); the hint is persistent state. Both are
prescribed.

---

## 4. Item 3 — Consistent channel label everywhere

### 4.1 The two shared helpers (add to `channels.rs`, near `channel_type_badge` at `channels.rs:368`)

```rust
/// The house channel label: "Title (id)" when a human title is stored, the
/// bare identifier otherwise. Every console surface that shows a channel
/// renders through this, so a channel never reads two different ways
/// (CHANNELS_TITLE_UX_SPEC §4). Both fields are escaped here.
pub(super) fn channel_label(title: Option<&str>, channel: &str) -> String {
    match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => format!("{} ({})", esc(t), esc(channel)),
        None => esc(channel),
    }
}

/// One `list_channels` scan → lowercase-keyed title map for pages that show
/// channel identifiers (dashboard readout, Posts, Subs, Sellers, Monitor).
/// Keys are `to_ascii_lowercase()` to mirror the column's NOCASE collation.
/// No new SQL — the existing read, shaped for lookup.
pub(super) fn channel_titles(state: &WebState) -> std::collections::HashMap<String, String> {
    state
        .db
        .list_channels(None)
        .into_iter()
        .filter_map(|row| {
            row.title
                .filter(|t| !t.trim().is_empty())
                .map(|t| (row.channel.to_ascii_lowercase(), t))
        })
        .collect()
}
```

Lookup idiom used at every call-site:

```rust
let titles = channel_titles(&state);
let label = channel_label(titles.get(&ch.to_ascii_lowercase()).map(String::as_str), &ch);
```

Scoping rationale: `list_channels(None)` is safe for all roles — seller-owned
rows effectively never exist in `channels` (nothing inserts them; sellers live
in `sellers.default_channel`), and sellers already see the global set in the
dashboard picker, so global titles are not new information for them.

### 4.2 Prescribed call-sites (label-only; value plumbing untouched)

| # | Site | Current code | Change |
|---|---|---|---|
| a | Dashboard "Default channel" stat — `src/web/dashboard.rs:78` (value bound at `:56`, `:97`) | `<span class="stat-value">{esc(&ch)}</span>` | `channel_label(titles.get(&ch.to_ascii_lowercase()).map(String::as_str), &ch)`; build `titles` once near `:56`. `"(none)"` fallback (`dashboard.rs:119-125`) passes through `channel_label(None, …)` unchanged |
| b | Dashboard composer channel `<select>` — `dashboard.rs:284-288` | `<option value="{0}">{0}{suffix}</option>` from `row.channel` | `<option value="{esc(&row.channel)}">{channel_label(row.title.as_deref(), &row.channel)}{suffix}</option>` — `row.title` is in hand in the loop |
| c | Dashboard composer, seller-own option — `dashboard.rs:290-299` | `{esc(&own)}` | same via `titles` map (`own` is by definition not in `seen`, so no title came from the loop) |
| d | Dashboard composer note — `dashboard.rs:308-312` | `"This will post to {esc(&ch)} as…"` | same map lookup feeding `channel_label`; sentence text unchanged |
| e | Posts table Channel cell — `src/web/content.rs:107-112` (`p.2`), header `:119` | `<td class="mono">{esc(&p.2)}</td>` | build `titles` once in `posts_page` before the `.iter()` at `:93`; cell renders `channel_label(...)`; stays `class="mono"` |
| f | Subs table Channel cell — `src/web/subs.rs:262` (`r.3`), plus the hidden revoke field `:270-274` | `<td class="mono">{esc(&r.3)}</td>` | cell label via `channel_label` + one `titles` map built in the page body before `rows.iter()` (`:249`). The hidden `name="channel"` value and the confirm string keep the RAW `esc(&r.3)` |
| g | Sellers admin table Channel cell — `src/web/admin_console.rs:410-415` | `<td class="mono">{esc(&ch)}</td>` | `channel_label` via `titles` map built once at `:406` |
| h | Monitor picker — `src/web/monitor.rs:80-84` options; caller `:276-282` | `<option value="{v}">{v}</option>` from `Vec<String>` | thread titles: caller collects `Vec<(String, Option<String>)>` from the `list_channels(None)` rows it already maps (`:276-281`); `monitor_controls` takes `channels: &[(String, Option<String>)]` and renders `channel_label(title.as_deref(), c)`. One caller (`monitor.rs:282`); the empty option `"No channels yet"` (`:72`) unchanged |

Rule for all of the above: **`Title (-100…)` when a title exists, bare
identifier when not — and NO titleless hint anywhere outside the Channels
page rows.** The hint names an action (Rename) that only exists on that page;
in pickers and dense tables it would be noise. `(default)` monitor/dashboard
sentinel options and badge suffixes (` · as user`, `dashboard.rs:274-280`)
are unchanged.

### 4.3 Audited and deliberately NOT changed

| Site | Why no change |
|---|---|
| Plans list — `content.rs:29-49`; dashboard plans — `dashboard.rs:142-150` | The `PlanRow` channel field (`db.rs:43`, `p.4`) is not rendered at all — the mono span is price + asset. Nothing to relabel; adding a channel column would be a page redesign (out of scope) |
| Settings default-channel input — `settings.rs:258,268`; dashboard plan-form channel input — `dashboard.rs:89`; bots admin channel input — `bots_admin.rs:296` | Editable form values: they must hold the raw identifier or the next submit corrupts it |
| Hidden inputs, confirm()/js_arg strings, aria-labels quoting channels (e.g. `channels.rs:157-168, 263-309`, `subs.rs:270`) | Machine-readable row keys and accessible action names; the visible label changes, these must not |
| Invite-link receipt — `channel_ops.rs:220` | Quotes the exact chat_id the operation targeted — an operation receipt, not a channel listing; listed as optional follow-up (§8) |
| Bot-side `/setchannel` / `/channels` replies — `src/handlers/commands.rs:469,3514` region | Telegram chat messages, no console markup/classes; out of territory |
| Auth/ownership checks — `src/web/mod.rs:586-629`, `content.rs:296-299` | Logic, not display |
| `bots_config.rs:124` | TOML config serialization, not UI |

---

## 5. Constraints for web (non-negotiable)

1. **No new routes.** `mod.rs` route table untouched.
2. **No handler-logic changes** beyond the §3.3 flag + §3.2 banner in the add
   flow. All other changes are view-layer (row builders, page bodies, the two
   helpers). The only new DB touch is the existing `list_channels` read.
3. **No DB changes.** No schema, no migration, no new `Db` methods;
   `set_channel_title` is not called anywhere new in this pass.
4. **`esc()` all dynamic data.** The hint strings are static literals;
   every title/channel value goes through `channel_label` (which escapes).
   Never build the label by string-concatenating raw values.
5. **Reuse existing classes only** (`help`, `alert alert-info`,
   `alert alert-success`, `badge`, `mono`). No CSS edits, no inline styles,
   no new components.
6. **Function bodies ≤ 100 lines.** Known pre-existing exception:
   `render_global_channel_row` (`channels.rs:239-354`) is already ≈116 lines;
   this spec adds ≤ 5 to it. Do NOT refactor it in this pass (churn risk);
   flag for a later cleanup instead. Every other touched function
   (`render_seller_channels` ≈39→≈52, `channels_add` ≈53→≈57,
   `dashboard_channel_options`, `posts_page`, `subs_page`, sellers table
   block, `monitor_controls`) stays well under budget.
7. **`channels.rs` size cap 800.** Current 712 lines; projected ≈775 after
   §2–§4 (helpers ≈+22, seller lookup ≈+12, banner/flag ≈+17, owner hint
   ≈+5, docs ≈+7). If implementation exceeds 795, move `channel_label` /
   `channel_titles` to `html.rs` (same visibility chain) rather than
   trimming the spec.
8. **Prefer additive changes over edits to asserted markup.** §6 lists the
   strings tests pin today; all prescriptions above keep every titleless
   render byte-identical to current output, so existing assertions hold
   without modification. New tests assert with `contains`, never with
   full-row equality.

## 6. Test-risk strings (verified against current test files)

| Location | Pinned string | Risk under this spec |
|---|---|---|
| `src/web/tests/channels.rs:153, 175, 179` | `<option value="@one">@one</option>`, `<option value="@mine">@mine</option>` | SAFE iff titleless `<option>` text stays the bare identifier (§4.2 rule). No test calls `set_channel_title`, so no test option is titled today. **If anyone later titles a test channel, these three assertions break by design** — update them to the new `Title (@one)` text |
| `src/web/tests/channels.rs:49` | `contains("@one") && contains("@two")` | SAFE — substring assertions; `Title (id)` and hints keep the identifier present |
| `src/web/tests/channels.rs:98` | `contains("bot-managed")` | SAFE — seller badge untouched |
| `src/web/tests/channels.rs:50, 60, 112, 149` | `default`, `cannot be removed`, `not yours to remove`, `(default)</option>` | SAFE — no changes nearby |
| `src/web/identity_tests.rs:859` | `<option value="@testchan" selected>@testchan</option>` (monitor picker) | SAFE today (`add_channel` at `:834` sets no title); same future-titling caveat as above. The direct `monitor_body` calls (`:836-913`) compile unchanged — `monitor_body`'s signature does not change; only `monitor_controls`' parameter type does (single production caller `monitor.rs:282`) |
| `src/web/smoke_tests.rs:166-181, 261-283` | `/channels` 200, `/channels/add` redirect | SAFE — status-only; `?added=1&notitle=1` still passes `is_redirection()`. No test asserts a `Location` containing `added=1` |
| `src/web/smoke_tests.rs:498-503` | `/channels/title` route | Untouched — that route is out of scope |

Additive test suggestions (Web): (1) owner add with unresolved title → redirect
query contains `notitle=1` and GET `/channels` contains both
`alert alert-info` and `no title yet — use Rename to name it`; (2) `?added=1`
alone still renders `Channel added.`; (3) titled row renders
`Tech Tips (-100…)` and no hint; (4) seller page contains
`no title stored for this channel yet`.

## 7. Acceptance checklist

- [ ] Owner Channels page: titleless row shows identifier + muted
      `no title yet — use Rename to name it` AFTER the badges, before
      row-actions; titled row shows `Title (-100…)` with no hint.
- [ ] Seller Channels page: same treatment with
      `no title stored for this channel yet`; `bot-managed` badge and Delete
      form byte-identical.
- [ ] Adding a channel while the bot cannot resolve it redirects to
      `?added=1&notitle=1` and renders exactly one info banner (owner copy
      with the Rename pointer; seller copy without).
- [ ] `?added=1` alone still renders the plain success banner; no page shows
      both add banners.
- [ ] Dashboard stat, composer options + note, Posts cell, Subs cell, Sellers
      cell, Monitor options all render `Title (-100…)` when titled, bare
      identifier when not; pickers and tables show no hint.
- [ ] Hidden inputs, confirm() copy, and aria-labels still carry the raw
      identifier everywhere.
- [ ] `cargo build && cargo test && cargo clippy -- -D warnings && cargo fmt
      --check` pass; `channels.rs` ≤ 800 lines; no new routes; no `Db` changes.
- [ ] Visual check in light AND dark theme: hint reads as muted footnote
      (`--ink-3`, 12px), never shouting; flex row wraps cleanly at 375px.

## 8. Follow-ups (non-blocking, out of this pass)

1. **Seller title capture:** the seller branch of `channels_add`
   (`channels.rs:416-423`) resolves `(canonical, title)` and discards the
   title; a `set_channel_title(&canonical, title.as_deref())` call there
   would fill the title whenever the seller's channel coincides with a
   global `channels` row (no-op otherwise). Handler-logic change → deferred.
2. **Seller Rename is a dead path** (`channels_rename` accepts sellers but
   `rename_channel` can never match a seller row): either hide seller intent
   or back it with a `sellers.default_channel` rename — owner's call.
3. **Invite-link receipt** (`channel_ops.rs:220`) could use `channel_label`.
4. **aria-labels** quoting channels could adopt the label too once the label
   helper has shipped and tests are updated.
