//! Cross-cutting moderation configuration surfaces (IA amendment §4).
//!
//! Three levels, one vocabulary: an owner-only global floor that nobody can
//! override, a seller-scoped Blocked words page, and the per-bot override that
//! lives inside the existing `/bots` Advanced disclosure. Every record is
//! read and written through `Db` methods that bind `seller_id` (and `bot_id`
//! for overrides) — this module never builds SQL and never trusts a seller id
//! supplied by a request. Feature-gated: all of its routes are registered only
//! when `moderation` is compiled in.

use super::*;
use crate::db::MODERATION_MAX_TERM_LEN;

/// The three modes at every level, with the plain-language descriptions the
/// spec requires. No jargon ("AI filter pipeline") reaches the UI.
const MODES: &[(&str, &str, &str)] = &[
    ("hide", "Hide", "matching messages are hidden"),
    ("warn", "Warn", "a warning is shown"),
    ("review", "Review", "matching messages are sent for review"),
];

fn is_mode(value: &str) -> bool {
    MODES.iter().any(|(v, _, _)| *v == value)
}

fn mode_label(value: &str) -> &'static str {
    MODES
        .iter()
        .find(|(v, _, _)| *v == value)
        .map(|(_, label, _)| *label)
        .unwrap_or("Warn")
}

/// Trim + validate exactly like the storage boundary (NUL, empty, over-long).
/// Returning the cleaned term means the value echoed on an error is the value
/// we would have stored, never the raw request string.
fn clean_term(raw: &str) -> Result<String, &'static str> {
    let term = raw.trim();
    if term.is_empty() {
        return Err("Enter a word before adding it.");
    }
    if term.len() > MODERATION_MAX_TERM_LEN {
        return Err("That word is too long. Use at most 200 characters.");
    }
    if term.contains('\0') {
        return Err("That word contains an invalid character.");
    }
    Ok(term.to_owned())
}

/// `<option>` set for a mode `<select>`. Callers own the `<label for>` with a
/// unique id; this helper only emits the three mode choices and their
/// plain-language descriptions.
fn mode_options(current: &str) -> String {
    MODES
        .iter()
        .map(|(value, label, help)| {
            format!(
                r#"<option value="{value}"{}>{} — {}</option>"#,
                if *value == current { " selected" } else { "" },
                esc(label),
                esc(help)
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The one-line mode glossary shared by each form. `aria-describedby` on the
/// selects points at the matching element.
fn mode_glossary(id: &str) -> String {
    format!(
        r#"<p class="help" id="{id}">Hide: matching messages are hidden. Warn: a warning is shown. Review: matching messages are sent for review.</p>"#
    )
}

/// Feedback rendered above a re-rendered form. Errors are focusable alerts so
/// a failed POST does not strand keyboard and screen-reader users; success is
/// a polite `role="status"` region.
fn feedback(error: Option<&str>, saved: bool) -> String {
    let mut out = String::new();
    if let Some(message) = error {
        out.push_str(&format!(
            r#"<div class="alert alert-error" role="alert" data-focus-on-load tabindex="-1">{}</div>"#,
            esc(message)
        ));
    }
    if saved {
        out.push_str(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Saved.</div>"#,
        );
    } else {
        // A live region is present even with nothing to announce, so a screen
        // reader has a stable target when a later in-place update occurs.
        out.push_str(r#"<div class="sr-only" role="status" aria-live="polite"></div>"#);
    }
    out
}

/// Session inputs kept on a validation error. The entered text and chosen
/// mode are never discarded.
#[derive(Clone, Default)]
struct Draft {
    term: String,
    mode: String,
}

impl Draft {
    fn new(term: &str, mode: &str) -> Self {
        Self {
            term: term.to_owned(),
            mode: mode.to_owned(),
        }
    }
}

#[allow(clippy::result_large_err)]
fn session_or_login(
    state: &WebState,
    headers: &axum::http::HeaderMap,
) -> Result<Session, Response> {
    session_from_headers(state, headers)
        .ok_or_else(|| Redirect::to(&state.url("/login")).into_response())
}

/// Owner floor gate. The page gate, not the nav item, is authoritative.
#[allow(clippy::result_large_err)]
fn owner_gate(state: &WebState, session: &Session) -> Result<(), Response> {
    if Role::from_role_str(&session.role).has_level(Role::Owner) {
        Ok(())
    } else {
        Err(forbidden_page(
            state,
            "Content policy",
            "The global content floor",
        ))
    }
}

/// Resolve the seller scope for a Blocked words read/write.
///
/// The web session is the ONLY source of seller identity. A request that
/// carries a `seller_id` field is a forged-scope attempt (the shipped forms
/// never emit one) and is refused with 403. Owners act in the primary shop
/// namespace (0), the same convention the rest of the console uses for
/// owner-authored business rows. An admin provisioned without a seller id is
/// treated the same as an owner (global scope 0); an admin WITH a seller id,
/// like a seller, is scoped to that id. A seller without one, or any lower
/// role, fails closed.
#[allow(clippy::result_large_err)]
fn seller_scope(
    state: &WebState,
    session: &Session,
    submitted_seller_id: Option<&str>,
) -> Result<i64, Response> {
    if submitted_seller_id.is_some_and(|value| !value.trim().is_empty()) {
        return Err(forbidden_page(state, "Blocked words", "Seller scoping"));
    }
    let role = Role::from_role_str(&session.role);
    if role.has_level(Role::Owner) {
        Ok(0)
    } else if role.has_level(Role::Admin) && session.seller_id.is_none() {
        // L10 LOW finding: an admin without a seller binding used to get a
        // 403 here. Admin outranks Seller, so the role gate passes; with no
        // seller id there is no tenant to scope to, so use the owner's 0.
        Ok(0)
    } else if role.has_level(Role::Seller) {
        session
            .seller_id
            .ok_or_else(|| forbidden_page(state, "Blocked words", "Seller scoping"))
    } else {
        Err(forbidden_page(state, "Blocked words", "Seller scoping"))
    }
}

/// Appeal status badge — a dot (from the shipped `.badge` component) before
/// the status WORD. Colour is decoration only; the text is authoritative.
fn appeal_status_badge(status: &str) -> String {
    let (label, class) = match status {
        "approved" => ("Approved", "badge badge-success"),
        "upheld" => ("Upheld", "badge"),
        _ => ("Pending", "badge badge-warning"),
    };
    format!(
        r#"<span class="{class}"><span class="sr-only">Status: </span>{}</span>"#,
        esc(label)
    )
}

// ---------------------------------------------------------------------------
// (a) Owner "Content policy" — non-overridable global floor
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(super) struct PolicyQuery {
    pub(super) seller_id: Option<String>,
}

fn content_policy_body(
    state: &WebState,
    error: Option<&str>,
    saved: bool,
    draft: Option<&Draft>,
) -> String {
    let terms = state.db.list_moderation_global_floor();
    let rows = if terms.is_empty() {
        r#"<li class="empty"><b>No floor terms yet.</b> Add a word to set the minimum every shop and bot must follow.</li>"#.to_string()
    } else {
        terms
            .iter()
            .map(|term| {
                format!(
                    r#"<li class="mod-row"><span class="mod-term mono">{}</span><span class="badge">{}</span>
  <form method="post" action="{}" class="inline-form">
    <input type="hidden" name="term" value="{}">
    <button type="submit" class="btn btn-sm btn-ghost">Remove</button>
  </form></li>"#,
                    esc(&term.term),
                    esc(mode_label(&term.mode)),
                    state.url("/content-policy/remove"),
                    esc(&term.term)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let draft = draft.cloned().unwrap_or_else(|| Draft {
        term: String::new(),
        mode: crate::db::MODERATION_FLOOR_MODE.to_owned(),
    });
    format!(
        r#"{feedback}<h1>Content policy <span class="sub">the floor for every shop and bot</span></h1>
<article class="panel">
  <p class="help">These words are the minimum for the whole platform. Shops and bots can add words or be stricter, but they cannot switch these off or loosen them.</p>
  <p class="help">Minimum enforcement: <span class="badge badge-success">{floor_mode}</span> — a shop can never handle a floor word less strictly than this.</p>
</article>
<article class="panel">
  <h3>Add a floor word</h3>
  <form method="post" action="{add_url}" class="mod-form">
    <div class="mod-field"><label for="policy-term">Word or phrase</label>
      <input id="policy-term" type="text" name="term" value="{term}" maxlength="{max_len}" required autocomplete="off">
      <span class="help">One word or phrase. Matching ignores case.</span>
    </div>
    <div class="mod-field"><label for="policy-mode">How to handle it</label>
      <select id="policy-mode" name="mode" aria-describedby="policy-mode-help">{mode_options}</select>
    </div>
    <button type="submit" class="btn">{button_label}</button>
  </form>
  {glossary}
</article>
<article class="panel"><h3>Floor words</h3><ul class="rows">{rows}</ul></article>"#,
        feedback = feedback(error, saved),
        floor_mode = esc(mode_label(crate::db::MODERATION_FLOOR_MODE)),
        add_url = state.url("/content-policy/add"),
        term = esc(&draft.term),
        max_len = MODERATION_MAX_TERM_LEN,
        mode_options = mode_options(&draft.mode),
        button_label = if error.is_some() && !draft.term.is_empty() {
            "Retry"
        } else {
            "Add a word"
        },
        glossary = mode_glossary("policy-mode-help"),
        rows = rows,
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn content_policy_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PolicyQuery>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if let Err(resp) = owner_gate(&state, &session) {
        return resp;
    }
    // A forged seller id is refused even on the read path.
    if q.seller_id.as_deref().is_some_and(|v| !v.trim().is_empty()) {
        return forbidden_page(&state, "Content policy", "Seller scoping");
    }
    Html(html_page(
        &state,
        Some(&session),
        "Content policy",
        &content_policy_body(&state, None, false, None),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct PolicyTermForm {
    pub(super) term: String,
    pub(super) mode: Option<String>,
    /// Never read — presence alone marks a forged-scope attempt.
    pub(super) seller_id: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn content_policy_add(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PolicyTermForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if let Err(resp) = owner_gate(&state, &session) {
        return resp;
    }
    if let Err(resp) = seller_scope(&state, &session, f.seller_id.as_deref()) {
        return resp;
    }
    let mode = f.mode.as_deref().unwrap_or("warn");
    let draft = Draft::new(&f.term, mode);
    let error = match clean_term(&f.term) {
        Err(e) => Some(e),
        Ok(_) if !is_mode(mode) => Some("Choose Hide, Warn, or Review."),
        Ok(term) if !state.db.add_moderation_global_floor_term(&term, mode) => {
            Some("We couldn't save that word. Check it and try again.")
        }
        Ok(_) => None,
    };
    let saved = error.is_none();
    Html(html_page(
        &state,
        Some(&session),
        "Content policy",
        &content_policy_body(&state, error, saved, Some(&draft)),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct PolicyRemoveForm {
    pub(super) term: String,
    pub(super) seller_id: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn content_policy_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PolicyRemoveForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    if let Err(resp) = owner_gate(&state, &session) {
        return resp;
    }
    if let Err(resp) = seller_scope(&state, &session, f.seller_id.as_deref()) {
        return resp;
    }
    if let Ok(term) = clean_term(&f.term) {
        state.db.remove_moderation_global_floor_term(&term);
    }
    Html(html_page(
        &state,
        Some(&session),
        "Content policy",
        &content_policy_body(&state, None, true, None),
    ))
    .into_response()
}

// ---------------------------------------------------------------------------
// (b) Seller "Blocked words" — primary surface with allow-list + appeal queue
// ---------------------------------------------------------------------------

fn blocked_words_rows(state: &WebState, seller_id: i64) -> String {
    let words = state.db.list_blocked_words(seller_id);
    if words.is_empty() {
        return r#"<li class="empty"><b>No blocked words yet.</b> Add a word to hide it, warn on it, or send matching messages for review.</li>"#.to_string();
    }
    words
        .iter()
        .map(|word| {
            let mode_id = format!("blocked-mode-{}", word.id);
            let term = esc(&word.term);
            format!(
                r#"<li class="mod-row"><span class="mod-term mono">{term}</span>
  <form method="post" action="{mode_url}" class="mod-row-form">
    <input type="hidden" name="term" value="{term}">
    <label class="sr-only" for="{mode_id}">How to handle {term}</label>
    <select id="{mode_id}" name="mode" aria-describedby="blocked-mode-help">{options}</select>
    <button type="submit" class="btn btn-sm btn-secondary">Update</button>
  </form>
  <form method="post" action="{remove_url}" class="inline-form">
    <input type="hidden" name="term" value="{term}">
    <button type="submit" class="btn btn-sm btn-ghost">Remove</button>
  </form></li>"#,
                term = term,
                mode_url = state.url("/blocked-words/mode"),
                mode_id = mode_id,
                options = mode_options(&word.mode),
                remove_url = state.url("/blocked-words/remove"),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn allow_words_rows(state: &WebState, seller_id: i64) -> String {
    let words = state.db.list_allow_words(seller_id);
    if words.is_empty() {
        return r#"<li class="empty"><b>No allowed words yet.</b> Add one if a word is being caught by mistake.</li>"#.to_string();
    }
    words
        .iter()
        .map(|word| {
            let term = esc(&word.term);
            format!(
                r#"<li class="mod-row"><span class="mod-term mono">{term}</span>
  <form method="post" action="{remove_url}" class="inline-form">
    <input type="hidden" name="term" value="{term}">
    <button type="submit" class="btn btn-sm btn-ghost">Remove</button>
  </form></li>"#,
                term = term,
                remove_url = state.url("/blocked-words/allow/remove"),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Appeal review queue. Owned by the seller scope and populated by the
/// bot-side flow (L8). Only an opaque request id and the status are shown —
/// no filtered message body and no buyer presence.
fn appeals_section(state: &WebState, seller_id: i64) -> String {
    let appeals = state.db.list_moderation_appeals(seller_id, None);
    let rows = if appeals.is_empty() {
        r#"<li class="empty"><b>No review requests yet.</b> Requests from your bots appear here with their status.</li>"#.to_string()
    } else {
        appeals
            .iter()
            .map(|appeal| {
                let bot_note = appeal
                    .bot_id
                    .map(|id| format!(" · bot {id}"))
                    .unwrap_or_default();
                let request = format!(
                    r#"<span class="mono">Request #{}{}</span>"#,
                    appeal.id, bot_note
                );
                let mut row = format!(
                    r#"<li class="mod-row">{}{}"#,
                    appeal_status_badge(&appeal.status),
                    request
                );
                if appeal.status == "pending" {
                    row.push_str(&format!(
                        r#"<form method="post" action="{url}" class="mod-row-form">
  <input type="hidden" name="appeal_id" value="{id}">
  <label class="sr-only" for="appeal-status-{id}">Decision for request #{id}</label>
  <select id="appeal-status-{id}" name="status">
    <option value="approved">Approved</option><option value="upheld">Upheld</option>
  </select>
  <button type="submit" class="btn btn-sm btn-secondary">Save decision</button>
</form>"#,
                        url = state.url("/blocked-words/appeal/status"),
                        id = appeal.id
                    ));
                }
                row.push_str("</li>");
                row
            })
            .collect::<Vec<_>>()
            .join("")
    };
    format!(
        r#"<article class="panel"><h3>Request review</h3>
<p class="help">When a buyer contests a filtered message, the request appears here. Only the request id and status are shown — never the message.</p>
<ul class="rows">{rows}</ul></article>"#,
        rows = rows
    )
}

fn blocked_words_body(
    state: &WebState,
    session: &Session,
    error: Option<&str>,
    saved: bool,
    draft: Option<&Draft>,
) -> String {
    // The scope is re-derived from the session for every render — the caller
    // never passes a request-supplied seller id into this function.
    let seller_id = seller_scope(state, session, None).unwrap_or_default();
    let draft = draft.cloned().unwrap_or_else(|| Draft {
        term: String::new(),
        mode: "warn".to_owned(),
    });
    let retry = error.is_some() && !draft.term.is_empty();
    format!(
        r#"{feedback}<h1>Blocked words <span class="sub">shop-wide rules</span></h1>
<article class="panel">
  <p class="help">Your shop builds on the owner's content policy. You can add words or make a word stricter, but you cannot loosen the owner's floor.</p>
</article>
<article class="panel">
  <h3>Add a word</h3>
  <form method="post" action="{add_url}" class="mod-form">
    <div class="mod-field"><label for="blocked-term">Word or phrase</label>
      <input id="blocked-term" type="text" name="term" value="{term}" maxlength="{max_len}" required autocomplete="off">
      <span class="help">One word or phrase. Matching ignores case.</span>
    </div>
    <div class="mod-field"><label for="blocked-mode-new">How to handle it</label>
      <select id="blocked-mode-new" name="mode" aria-describedby="blocked-mode-help">{mode_options}</select>
    </div>
    <button type="submit" class="btn">{button_label}</button>
  </form>
  {glossary}
</article>
<article class="panel"><h3>Your blocked words</h3><ul class="rows">{rows}</ul></article>
<article class="panel">
  <h3>Always allow</h3>
  <p class="help">Words here are never filtered for your shop, even if a blocked word matches. The owner's floor still applies.</p>
  <form method="post" action="{allow_url}" class="mod-form">
    <div class="mod-field"><label for="allow-term">Allowed word or phrase</label>
      <input id="allow-term" type="text" name="term" value="{allow_term}" maxlength="{max_len}" required autocomplete="off">
    </div>
    <button type="submit" class="btn">Add an allowed word</button>
  </form>
  <ul class="rows">{allow_rows}</ul>
</article>
{appeals}"#,
        feedback = feedback(error, saved),
        add_url = state.url("/blocked-words/add"),
        term = esc(&draft.term),
        max_len = MODERATION_MAX_TERM_LEN,
        mode_options = mode_options(&draft.mode),
        button_label = if retry { "Retry" } else { "Add a word" },
        glossary = mode_glossary("blocked-mode-help"),
        rows = blocked_words_rows(state, seller_id),
        allow_url = state.url("/blocked-words/allow"),
        allow_term = esc(&draft.term),
        allow_rows = allow_words_rows(state, seller_id),
        appeals = appeals_section(state, seller_id),
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PolicyQuery>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, q.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let _ = seller_id; // the body re-derives it from the same session
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, None, false, None),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct WordForm {
    pub(super) term: String,
    pub(super) mode: Option<String>,
    /// Never read — presence alone marks a forged-scope attempt.
    pub(super) seller_id: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_add(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WordForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let mode = f.mode.as_deref().unwrap_or("warn");
    let draft = Draft::new(&f.term, mode);
    let error = match clean_term(&f.term) {
        Err(e) => Some(e),
        Ok(_) if !is_mode(mode) => Some("Choose Hide, Warn, or Review."),
        Ok(term) if state.db.get_blocked_word(seller_id, &term).is_some() => {
            Some("That word is already in your blocked list.")
        }
        Ok(term) if !state.db.add_blocked_word(seller_id, &term, mode, true) => {
            Some("We couldn't save that word. Check it and try again.")
        }
        Ok(_) => None,
    };
    let saved = error.is_none();
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, error, saved, Some(&draft)),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_set_mode(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WordForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let mode = f.mode.as_deref().unwrap_or("warn");
    let error = match clean_term(&f.term) {
        Err(e) => Some(e),
        Ok(_) if !is_mode(mode) => Some("Choose Hide, Warn, or Review."),
        Ok(term) if !state.db.set_blocked_word_mode(seller_id, &term, mode) => {
            Some("We couldn't update that word. Try again.")
        }
        Ok(_) => None,
    };
    let draft = Draft::new(&f.term, mode);
    let saved = error.is_none();
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, error, saved, Some(&draft)),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct TermForm {
    pub(super) term: String,
    /// Never read — presence alone marks a forged-scope attempt.
    pub(super) seller_id: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<TermForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Ok(term) = clean_term(&f.term) {
        state.db.remove_blocked_word(seller_id, &term);
    }
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, None, true, None),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_allow(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<TermForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let error = match clean_term(&f.term) {
        Err(e) => Some(e),
        Ok(term) if state.db.get_allow_word(seller_id, &term).is_some() => {
            Some("That word is already on your always-allow list.")
        }
        Ok(term) if !state.db.add_allow_word(seller_id, &term) => {
            Some("We couldn't save that word. Check it and try again.")
        }
        Ok(_) => None,
    };
    let draft = Draft {
        term: f.term.clone(),
        mode: "warn".to_owned(),
    };
    let saved = error.is_none();
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, error, saved, Some(&draft)),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_allow_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<TermForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    if let Ok(term) = clean_term(&f.term) {
        state.db.remove_allow_word(seller_id, &term);
    }
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, None, true, None),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct AppealDecisionForm {
    pub(super) appeal_id: String,
    pub(super) status: String,
    /// Never read — presence alone marks a forged-scope attempt.
    pub(super) seller_id: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn blocked_words_appeal_status(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AppealDecisionForm>,
) -> impl IntoResponse {
    let session = match session_or_login(&state, &headers) {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let seller_id = match seller_scope(&state, &session, f.seller_id.as_deref()) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let error = match f.appeal_id.trim().parse::<i64>() {
        Err(_) => Some("That review request could not be found."),
        Ok(id) if state.db.get_moderation_appeal(seller_id, id).is_none() => {
            Some("That review request could not be found.")
        }
        Ok(id)
            if !state
                .db
                .set_moderation_appeal_status(seller_id, id, &f.status) =>
        {
            Some("Choose Approved or Upheld, then try again.")
        }
        Ok(_) => None,
    };
    Html(html_page(
        &state,
        Some(&session),
        "Blocked words",
        &blocked_words_body(&state, &session, error, error.is_none(), None),
    ))
    .into_response()
}

// ---------------------------------------------------------------------------
// (c) Per-bot override — rendered inside the existing /bots Advanced details
// ---------------------------------------------------------------------------

/// Owner-managed bots live in the primary-shop namespace (0), matching the
/// rest of the owner console. Both keys are always bound by the Db methods.
fn override_seller_id() -> i64 {
    0
}

/// The Advanced moderation block for one bot card. Default is inheritance
/// ("Using your shop's rules") with one secondary "Customize" action. Custom
/// rules reveal a stricter-only word list; returning to inheritance preserves
/// the typed words in storage (Db keeps the rows and flips the sentinel).
pub(super) fn bot_override_advanced_html(state: &WebState, b: &crate::db::BotRecord) -> String {
    let seller_id = override_seller_id();
    let inherits = state.db.bot_override_inherits(seller_id, b.id);
    let url = state.url(&format!("/bots/{}/moderation/inherit", b.id));
    let mut out = format!(
        r#"<div class="moderation-override" id="bot-moderation-{id}">
  <h4>Moderation for this bot</h4>
  <p class="help">Shop rules apply here by default. A bot rule can add to them or make a word stricter, never looser than your shop or the owner's floor.</p>"#,
        id = b.id
    );
    if inherits {
        out.push_str(
            r#"<p class="empty moderation-state"><b>Using your shop's rules.</b> No separate rules for this bot.</p>"#,
        );
        out.push_str(&format!(
            r#"<form method="post" action="{url}" class="inline-form">
    <input type="hidden" name="bot_id" value="{id}">
    <input type="hidden" name="inherits" value="false">
    <button type="submit" class="btn btn-sm btn-secondary">Customize</button>
  </form>"#,
            id = b.id
        ));
    } else {
        out.push_str(
            r#"<p class="help">Custom rules for this bot: <span class="badge badge-success">Custom rules for this bot</span></p>"#,
        );
        let terms = state.db.list_bot_override_terms(seller_id, b.id);
        if terms.is_empty() {
            out.push_str(
                r#"<p class="empty moderation-state"><b>No bot-specific words yet.</b> Add one below, or return to your shop's rules.</p>"#,
            );
        } else {
            out.push_str("<ul class=\"rows\">");
            for term in &terms {
                let mode_id = format!("bot-mod-mode-{}-{}", b.id, term.id);
                out.push_str(&format!(
                    r#"<li class="mod-row"><span class="mono">{term}</span>
  <form method="post" action="{mode_url}" class="mod-row-form">
    <input type="hidden" name="term" value="{term}">
    <label class="sr-only" for="{mode_id}">How to handle {term} for this bot</label>
    <select id="{mode_id}" name="mode" aria-describedby="bot-mod-help-{id}">{options}</select>
    <button type="submit" class="btn btn-sm btn-secondary">Update</button>
  </form>
  <form method="post" action="{remove_url}" class="inline-form">
    <input type="hidden" name="term" value="{term}">
    <button type="submit" class="btn btn-sm btn-ghost">Remove</button>
  </form></li>"#,
                    term = esc(&term.term),
                    mode_url = state.url(&format!("/bots/{}/moderation/mode", b.id)),
                    mode_id = mode_id,
                    options = mode_options(&term.mode),
                    id = b.id,
                    remove_url = state.url(&format!("/bots/{}/moderation/remove", b.id)),
                ));
            }
            out.push_str("</ul>");
        }
        out.push_str(&format!(
            r#"<p class="help" id="bot-mod-help-{id}">Hide: matching messages are hidden. Warn: a warning is shown. Review: matching messages are sent for review.</p>
  <form method="post" action="{add_url}" class="mod-form">
    <div class="mod-field"><label for="bot-mod-new-{id}">Word or phrase for this bot</label>
      <input id="bot-mod-new-{id}" type="text" name="term" maxlength="{max_len}" required autocomplete="off">
    </div>
    <div class="mod-field"><label for="bot-mod-new-mode-{id}">How to handle it</label>
      <select id="bot-mod-new-mode-{id}" name="mode">{options}</select>
    </div>
    <button type="submit" class="btn">Add word for this bot</button>
  </form>"#,
            id = b.id,
            add_url = state.url(&format!("/bots/{}/moderation/add", b.id)),
            max_len = MODERATION_MAX_TERM_LEN,
            options = mode_options("warn")
        ));
        out.push_str(&format!(
            r#"<form method="post" action="{url}" class="inline-form moderation-restore">
    <input type="hidden" name="bot_id" value="{id}">
    <input type="hidden" name="inherits" value="true">
    <button type="submit" class="btn btn-sm btn-ghost">Use your shop's rules</button>
  </form>"#,
            id = b.id
        ));
    }
    out.push_str("</div>");
    out
}

#[derive(Deserialize)]
pub(super) struct BotOverrideForm {
    pub(super) bot_id: Option<String>,
    pub(super) inherits: Option<String>,
    pub(super) term: Option<String>,
    pub(super) mode: Option<String>,
    /// Never read — presence alone marks a forged-scope attempt.
    pub(super) seller_id: Option<String>,
}

/// Shared gate for per-bot override writes: owner-only session, live bot id,
/// and no request-supplied seller id. `bot_id` comes from the URL path; a
/// mismatching hidden field is refused rather than silently trusted.
#[allow(clippy::result_large_err)]
fn bot_override_gate(
    state: &WebState,
    headers: &axum::http::HeaderMap,
    bot_id: i64,
    f: &BotOverrideForm,
) -> Result<Session, Response> {
    let session = session_or_login(state, headers)?;
    owner_gate(state, &session)?;
    seller_scope(state, &session, f.seller_id.as_deref())?;
    if state.db.get_bot(bot_id).is_none() {
        return Err(forbidden_page(state, "Bots", "Per-bot moderation"));
    }
    if f.bot_id
        .as_deref()
        .is_some_and(|raw| raw.trim().parse::<i64>().ok() != Some(bot_id))
    {
        return Err(forbidden_page(state, "Bots", "Per-bot moderation"));
    }
    Ok(session)
}

fn bots_redirect(state: &WebState, bot_id: i64) -> Response {
    Redirect::to(&state.url(&format!("/bots?saved=1&bot={bot_id}"))).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn bot_override_inherit(
    State(state): State<WebState>,
    Path(bot_id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotOverrideForm>,
) -> impl IntoResponse {
    if let Err(resp) = bot_override_gate(&state, &headers, bot_id, &f) {
        return resp;
    }
    let inherits = f.inherits.as_deref() != Some("false");
    state
        .db
        .set_bot_override_inherits(override_seller_id(), bot_id, inherits);
    bots_redirect(&state, bot_id)
}

#[tracing::instrument(skip_all)]
pub(super) async fn bot_override_add(
    State(state): State<WebState>,
    Path(bot_id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotOverrideForm>,
) -> impl IntoResponse {
    if let Err(resp) = bot_override_gate(&state, &headers, bot_id, &f) {
        return resp;
    }
    let mode = f.mode.as_deref().unwrap_or("warn");
    let term = match clean_term(f.term.as_deref().unwrap_or("")) {
        Ok(t) if is_mode(mode) => t,
        _ => return bots_redirect(&state, bot_id),
    };
    // `add_bot_override_term` flips the sentinel to custom, so one action is
    // enough; the resolver still clamps the mode to the shop/owner floor.
    state
        .db
        .add_bot_override_term(override_seller_id(), bot_id, &term, mode);
    bots_redirect(&state, bot_id)
}

#[tracing::instrument(skip_all)]
pub(super) async fn bot_override_set_mode(
    State(state): State<WebState>,
    Path(bot_id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotOverrideForm>,
) -> impl IntoResponse {
    if let Err(resp) = bot_override_gate(&state, &headers, bot_id, &f) {
        return resp;
    }
    let mode = f.mode.as_deref().unwrap_or("warn");
    if let (Ok(term), true) = (clean_term(f.term.as_deref().unwrap_or("")), is_mode(mode)) {
        // No dedicated mode setter exists at this layer; the idempotent add
        // upserts the mode for the same (seller_id, bot_id, term) key.
        state
            .db
            .add_bot_override_term(override_seller_id(), bot_id, &term, mode);
    }
    bots_redirect(&state, bot_id)
}

#[tracing::instrument(skip_all)]
pub(super) async fn bot_override_remove(
    State(state): State<WebState>,
    Path(bot_id): Path<i64>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotOverrideForm>,
) -> impl IntoResponse {
    if let Err(resp) = bot_override_gate(&state, &headers, bot_id, &f) {
        return resp;
    }
    if let Ok(term) = clean_term(f.term.as_deref().unwrap_or("")) {
        state
            .db
            .remove_bot_override_term(override_seller_id(), bot_id, &term);
    }
    bots_redirect(&state, bot_id)
}
