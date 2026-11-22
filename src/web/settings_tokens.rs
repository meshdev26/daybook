//! Per-bot token plumbing for the Settings connection panel
//! (SETTINGS_PER_BOT_TOKEN_SPEC §3/§5): mode detection, the DB-bot ×
//! boot-config row join, the multi-bot panel markup, and the multi-bot save
//! branch. Split from settings.rs (the §9 "approaching 800" prescription —
//! applied for real once the §3.2 markup volume was known) so both files
//! stay well under the 800-line budget.

use super::*;

/// True when the running process resolved its bots from a bot config file
/// (`BOTS_CONFIG` / `BOTS_JSON`) — the Settings page renders the per-bot
/// panel. A *set-but-broken* file resolved as legacy at boot, so this is
/// false there: the panel must describe what the process actually uses.
pub(super) fn multi_bot_mode(state: &WebState) -> bool {
    !state.cfg.bots_legacy
}

/// One row of the multi-bot token panel: a DB bot (`db.list_bots()` — the
/// same row source as Operate → Bots, so the two surfaces list 1:1) joined
/// with its boot-config entry from `cfg.bots`.
pub(super) struct BotTokenRow {
    pub(super) record: crate::db::BotRecord,
    /// NAME of the env var this row's token is written back to (state A).
    /// `None` for an inline token in the config file (state B) and for rows
    /// with no config match (state C) — `matched` tells those two apart;
    /// both render without an input (§3.2 states).
    pub(super) token_env: Option<String>,
    /// Boot-resolved token presence for the §3.3 badge. Unmatched rows fall
    /// back to the BotManager derivation (the bots_admin.rs badge idiom).
    pub(super) configured: bool,
    /// The boot config carried an entry for this bot's key.
    pub(super) matched: bool,
}

/// `db.list_bots()` joined by `key` with the boot snapshot `cfg.bots`. The
/// badge reads `cfg.bots[i].token` emptiness (boot-resolved); a row the boot
/// config does not know falls back to the loaded `BotContext` token check.
pub(super) fn bot_token_rows(state: &WebState) -> Vec<BotTokenRow> {
    state
        .db
        .list_bots()
        .into_iter()
        .map(
            |record| match state.cfg.bots.iter().find(|c| c.key == record.key) {
                Some(c) => BotTokenRow {
                    token_env: c.token_env.clone(),
                    configured: !c.token.trim().is_empty(),
                    matched: true,
                    record,
                },
                None => BotTokenRow {
                    configured: state
                        .bots
                        .get(record.id)
                        .map(|ctx| !ctx.bot.token().trim().is_empty())
                        .unwrap_or(false),
                    token_env: None,
                    matched: false,
                    record,
                },
            },
        )
        .collect()
}

/// One composed token write: the env var to manage, the submitted (still
/// unvalidated) value, and the bot's display name for the banner.
pub(super) struct TokenWrite {
    pub(super) var: String,
    pub(super) value: String,
    pub(super) bot_name: String,
}

/// Manual `Debug` in place of the derive (verification P3-1): `value` carries a raw
/// bot token, so the derived render would make a future
/// `tracing::debug!(?writes)` a secret-leak waiting to happen. The value is
/// redacted unconditionally — no verbosity flag ever turns it back on.
impl std::fmt::Debug for TokenWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenWrite")
            .field("var", &self.var)
            .field("bot_name", &self.bot_name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Pure composition unit behind the multi-bot save (§5.1–§5.2): for each
/// state-A row, a non-blank `bot_{key}` form field yields one write, in row
/// order; blanks write nothing (the `non_blank` contract). Two non-blank
/// rows resolving to the SAME var with different values are rejected —
/// dotenvy is first-wins, so a duplicate line would silently keep one of
/// the two; identical values collapse to one entry. Network-free: token
/// validation happens in the handler before any write (§5.3). A key that is
/// not a safe HTML name degrades the row to state C and is never composed.
pub(super) fn token_env_updates(
    state: &WebState,
    form: &HashMap<String, String>,
) -> Result<Vec<TokenWrite>, String> {
    let mut out: Vec<TokenWrite> = Vec::new();
    for row in bot_token_rows(state) {
        let Some(var) = row
            .token_env
            .clone()
            .filter(|_| key_is_slug(&row.record.key))
        else {
            continue;
        };
        let Some(value) = non_blank_in(form, &format!("bot_{}", row.record.key)) else {
            continue;
        };
        match out.iter_mut().find(|w| w.var == var) {
            Some(w) if w.value != value => {
                return Err(format!(
                    "Two bots share the setting <code>{}</code> — fix the bot config file first. Nothing was saved.",
                    esc(&var)
                ));
            }
            // Identical value behind a shared var: collapse to one entry.
            Some(_) => {}
            None => out.push(TokenWrite {
                var,
                value: value.to_string(),
                bot_name: bot_display_name(&row.record).to_string(),
            }),
        }
    }
    Ok(out)
}

/// A bot key safe to interpolate into HTML id/name attributes — the same
/// strict-slug guard as `js_key` (bots_admin.rs); anything else degrades the
/// row to state C (§3.2) so the key can never reach markup or JS contexts.
pub(super) fn key_is_slug(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The multi-bot branch of the connection panel (SETTINGS_PER_BOT_TOKEN_SPEC
/// §3.2): the mode line, one write-only token row per DB bot, then the
/// owner/channel fields with the scoped honesty note and the unchanged
/// Crypto Pay field. Tokens stay write-only in every row state.
pub(super) fn multi_bot_connection_panel(state: &WebState) -> String {
    let rows = bot_token_rows(state)
        .iter()
        .map(token_row_html)
        .collect::<Vec<_>>()
        .join("\n");
    let crypto_badge = crypto_badge_of(state);
    let owner_id_value = owner_id_value_of(state);
    let default_channel_value = state.db.default_channel().unwrap_or_default();
    let scope_note = format!(
        r#"<span class="help">With a bot config file, each bot's owner ID and channel live on its <a href="{}">Bots card</a> instead; these two fields only apply in single-bot mode.</span>"#,
        state.url("/bots")
    );
    format!(
        r#"<details open id="connection"><summary>Bot connection</summary>
<article class="panel">
<p class="help" id="connection-mode"><b>Multi-bot mode.</b> Your bots come from a bot config file (<code>BOTS_CONFIG</code> / <code>BOTS_JSON</code>). Each bot has its own token field below; its card lives under <a href="{bots_url}">Operate → Bots</a>.</p>
<form method="post" action="{save_url}">
<input type="hidden" name="section" value="connection">
{rows}
{recipe}
<label for="owner_id">Owner Telegram user ID</label><input id="owner_id" type="text" name="owner_id" inputmode="numeric" value="{owner_val}" placeholder="123456789"><span class="help">Your own Telegram account number (@userinfobot replies with it). Leave blank to keep the current one.</span>
<label for="default_channel">Default channel</label><input id="default_channel" type="text" name="default_channel" value="{channel_val}" placeholder="@yourchannel"><span class="help">Where posts and paid access go when nothing else is set — for example @myshopnews. Leave blank to keep the current one.</span>
{scope_note}
<label for="settings-crypto-token">Crypto Pay token {crypto_badge}</label>
<span class="secret-field"><input type="password" id="settings-crypto-token" name="crypto_token" autocomplete="new-password" placeholder="from @CryptoBot → Crypto Pay → Create App">{toggle}</span><span class="help">From @CryptoBot → Crypto Pay → Create App. Lets the console verify payments automatically. Leave blank to keep the current token.</span>
<button type="submit">Save settings</button>
</form>
<p class="help">Blank fields change nothing. Saving writes your config file — changes go live when the app restarts.</p>
<p class="help">AI provider and usage live on the <a href="{ai_url}">AI page</a>.</p>
<p class="help">REST API keys are managed on the <a href="{keys_url}">API keys</a> page.</p>
</article></details>"#,
        bots_url = state.url("/bots"),
        save_url = state.url("/settings/save"),
        rows = rows,
        recipe = BOTFATHER_RECIPE_SETTINGS,
        owner_val = esc(&owner_id_value),
        channel_val = esc(&default_channel_value),
        scope_note = scope_note,
        crypto_badge = crypto_badge,
        toggle = SECRET_TOGGLE,
        ai_url = state.url("/ai"),
        keys_url = state.url("/keys"),
    )
}

/// One bot's token row (§3.2): state A a write-only secret field addressed
/// `bot_{key}`, state B an inline-token honesty row, state C a not-yet-loaded
/// honesty row (also the degradation for a non-slug key). The console never
/// renders a current token — placeholder only, in any state.
fn token_row_html(row: &BotTokenRow) -> String {
    let name = esc(bot_display_name(&row.record));
    let key = esc(&row.record.key);
    let badge = if row.configured {
        r#"<span class="badge badge-success">configured</span>"#
    } else {
        r#"<span class="badge badge-warning">not set</span>"#
    };
    let slug = key_is_slug(&row.record.key);
    match (row.matched, row.token_env.as_deref(), slug) {
        // State A: the boot config carries a named setting to write back to.
        (true, Some(var), true) => format!(
            r#"<label for="settings-bot-token-{key}">Bot token — {name} {badge}</label>
<span class="secret-field"><input type="password" id="settings-bot-token-{key}" name="bot_{key}" autocomplete="new-password" placeholder="Paste a new token from @BotFather — the current one is never shown">{toggle}</span>
<span class="help">Saved to the <code>{var}</code> setting in your bot config file. Leave blank to keep the current one — the console never shows it.</span>"#,
            key = key,
            name = name,
            badge = badge,
            var = esc(var),
            toggle = SECRET_TOGGLE,
        ),
        // State B: the token is inline in the config file — there is no
        // setting name to write to, so the row offers no input at all.
        (true, None, _) => format!(
            r#"<label>Bot token — {name} {badge}</label>
<span class="help">This bot's token is set directly in your bot config file — edit it there. The console cannot write it (there is no setting name to write to).</span>"#,
            name = name,
            badge = badge,
        ),
        // State C: no config match (or an unsafe key) — honest no-input row.
        _ => format!(
            r#"<label>Bot token — {name} {badge}</label>
<span class="help">The console can't see this bot's config entry yet. Restart to load your bot config, then set its token here.</span>"#,
            name = name,
            badge = badge,
        ),
    }
}

/// Multi-bot branch of the connection save (SETTINGS_PER_BOT_TOKEN_SPEC §5):
/// validate every candidate token BEFORE any write (no partial writes, ever),
/// then one `write_env_file_replacing` call whose managed set is exactly the
/// keys written this save. The legacy `write_env_file` would duplicate every
/// `BOT_*` line (its fixed preserve-set excludes them) and dotenvy's
/// first-wins would silently keep the stale one at boot.
pub(super) async fn settings_save_multi_bot(
    state: &WebState,
    session: &Session,
    section: &str,
    form: &HashMap<String, String>,
) -> Response {
    // §5.6: a stale bookmark or crafted POST must not resurrect the shadowed
    // legacy write — the single-bot field is not even rendered in this mode.
    if non_blank_in(form, "bot_token").is_some() {
        return settings_error(
            state,
            session,
            Some(section),
            "Single-bot token fields are not used in multi-bot mode — set each bot's token below. Nothing was saved.",
        );
    }
    // Compose the per-bot portion of the managed set (pure, network-free).
    let writes = match token_env_updates(state, form) {
        Ok(w) => w,
        Err(msg) => return settings_error(state, session, Some(section), &msg),
    };
    // F2.5: same verdicts as the single-token surface, per bot, in row order.
    for w in &writes {
        if let Err(e) = validate_token_username(&w.value).await {
            return settings_error(
                state,
                session,
                Some(section),
                &format!("{} — {}", esc(&w.bot_name), token_error_copy(&e)),
            );
        }
    }
    let mut updates: Vec<(String, Option<String>)> = writes
        .iter()
        .map(|w| (w.var.clone(), Some(w.value.clone())))
        .collect();
    // Legacy env keys join the same managed set (§5.1) — one atomic write.
    if let Some(o) = non_blank_in(form, "owner_id") {
        match o.parse::<i64>() {
            Ok(v) if v > 0 => updates.push(("OWNER_ID".to_string(), Some(v.to_string()))),
            _ => {
                return settings_error(
                    state,
                    session,
                    Some(section),
                    "Owner ID must be the number @userinfobot replies with — for example 123456789. Nothing was saved.",
                );
            }
        }
    }
    if let Some(c) = non_blank_in(form, "default_channel") {
        updates.push(("DEFAULT_CHANNEL".to_string(), Some(c.to_string())));
    }
    if let Some(c) = non_blank_in(form, "crypto_token") {
        updates.push(("CRYPTO_PAY_TOKEN".to_string(), Some(c.to_string())));
    }
    if updates.is_empty() {
        // Nothing to change — do not touch the config file as a side effect
        // of an all-blank submit (same contract as the legacy branch).
        return settings_response(
            state,
            session,
            Some(section),
            r#"<div class="alert alert-info" role="status" aria-live="polite">Nothing to save — all fields were blank.</div>"#
                .to_string(),
        );
    }
    let managed: Vec<(&str, Option<&str>)> = updates
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_deref()))
        .collect();
    match crate::config::write_env_file_replacing(&managed) {
        Ok(_) => {
            // The file changed, but the process reads its config snapshot
            // once at boot: flag the standing restart banner exactly once.
            state.db.set_config("restart_required", "1");
            let banner = if writes.is_empty() {
                "Saved — restart to apply.".to_string()
            } else {
                let names = writes
                    .iter()
                    .map(|w| esc(&w.bot_name))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Saved — restart to apply. Tokens set for: {names}.")
            };
            settings_response(
                state,
                session,
                Some(section),
                format!(
                    r#"<div class="alert alert-success" role="status" aria-live="polite">{banner}</div>
<p class="help">Values are written to your config file; the running process keeps the settings snapshot it started with, so restart to apply them.</p>"#,
                ),
            )
        }
        Err(e) => settings_error(
            state,
            session,
            Some(section),
            &format!(
                "Failed to write the config file: {}. Nothing was saved.",
                esc(&e.to_string())
            ),
        ),
    }
}
