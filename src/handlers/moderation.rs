//! Cross-cutting moderation hooks (multibot foundation plan, Step 4.2).
//!
//! This module is the single source of truth for the *decision* half of both
//! hooks. It only ever READS policy through
//! [`Db::resolve_effective_policy`] — it has no caller path to the owner
//! floor writers (`add/remove_moderation_global_floor_term`), which stay an
//! L10 carry-forward. Every decision, including a pass, is appended through
//! [`Db::log_moderation_decision`] with an **opaque** message reference and a
//! short reason code. No message body, username or secret is ever logged or
//! persisted here: the reference is a one-way hash of the context plus text,
//! and `term` is a policy term from the resolved policy, never user content.
//!
//! # Fail-closed floor
//!
//! `EffectiveModerationPolicy::enforcement_degraded` means moderation storage
//! was unavailable at startup or the persistent floor could not be read. An
//! empty term list in that state is **not** "the message is safe" — it means
//! enforcement is unavailable. `decide()` therefore short-circuits to
//! [`ModerationMode::Hold`] before evaluating any term. Both hooks treat Hold
//! as "do not route / do not send" and emit a `tracing::warn!` with scope and
//! the opaque reference only.
//!
//! # Mode mapping
//!
//! The three plain-language modes keep the same meaning on both paths:
//!
//! * `warn`  — softest. Recorded in the audit, but the message is allowed
//!   through (inbound: routed normally; outbound: sent). Surfacing the actual
//!   warning text to a user is part of the deferred appeal conversation.
//! * `review` — held. Inbound: withheld from normal routing and recorded for
//!   the review queue. Outbound: withheld from sending.
//! * `hide`  — hidden. Inbound: dropped without a reply. Outbound: blocked.
//!
//! There is no AI classifier call anywhere in this module — deterministic
//! local matching is the floor; an optional external classifier is a later
//! enhancement and never a replacement.
//!
//! # Seller scope decision for the owner-identity sender
//!
//! [`crate::identity::UserContext`] is the single operator personal account
//! and carries no bot id or per-session seller. Its sends are evaluated with
//! `seller_id = 0, bot_id = None`, i.e. the **global floor only** — no seller
//! rules and no per-bot override can be attributed to an owner-identity send.
//! Bot-context sends use the sole seller attributed to the receiving bot
//! (via `Db::sellers_for_bot`), falling back to the global namespace `0` for
//! the shared/primary bot where the seller cannot be derived from the update.

use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::db::{Db, EffectiveModerationPolicy, moderation_mode_severity};

/// Fixed error text used when the outbound gate holds content. `SendError`
/// has no dedicated variant and its exhaustive match lives outside this
/// rung's territory, so the hold rides in the existing `Transport(String)`
/// arm with a stable, content-free marker that callers/tests can identify.
pub const OUTBOUND_HOLD_ERROR: &str = "outbound content held by content policy (moderation_hold)";

/// The single, content-free error returned by the pre-transport gate
/// [`gate_outbound`]. Its `Display` is the stable `moderation_hold` marker, so
/// every caller can surface an opaque, uniform signal (via `?`/`map_err`)
/// without ever echoing the held body, a username, or a secret. It implements
/// `std::error::Error` so it converts straight into the handlers'
/// `Box<dyn Error>` result type and into `SendError::Transport` at the sender
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundHold;

impl fmt::Display for OutboundHold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(OUTBOUND_HOLD_ERROR)
    }
}

impl std::error::Error for OutboundHold {}

/// Central pre-transport outbound gate. **Every** content-bearing send path
/// must call this (or route through the gated
/// [`crate::identity::ContentSender::send_text`]) before touching a Telegram
/// transport. The policy decision has exactly one implementation here:
/// [`evaluate_outbound`] resolves + decides + audits, and this wrapper turns a
/// blocking verdict into [`OutboundHold`]. Degraded enforcement holds, as
/// before. Passing/warn content returns `Ok(())` with unchanged behavior.
pub fn gate_outbound(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> Result<(), OutboundHold> {
    if evaluate_outbound(db, seller_id, bot_id, text).blocks_outbound_send() {
        Err(OutboundHold)
    } else {
        Ok(())
    }
}

/// Resolution of one evaluated text, independent of *where* it is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModerationMode {
    /// No term matched and enforcement was healthy.
    Pass,
    /// A `warn`-mode term matched; record and allow.
    Warn,
    /// A `review`-mode term matched; hold from normal routing/sending.
    Review,
    /// A `hide`-mode term matched; suppress entirely.
    Hide,
    /// Enforcement is degraded; fail closed (hold, never pass-through).
    Hold,
}

impl ModerationMode {
    fn from_policy_mode(mode: &str) -> Self {
        match mode {
            "hide" => Self::Hide,
            "review" => Self::Review,
            _ => Self::Warn, // unknown stored values clamp to the floor mode
        }
    }

    /// True when an inbound message must be withheld from command/text routing.
    pub fn blocks_inbound_routing(self) -> bool {
        matches!(self, Self::Hide | Self::Review | Self::Hold)
    }

    /// True when an outbound send must be refused at the transport boundary.
    pub fn blocks_outbound_send(self) -> bool {
        matches!(self, Self::Hide | Self::Review | Self::Hold)
    }

    /// Short, machine-readable action code for the audit row.
    pub fn action_code(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Review => "review",
            Self::Hide => "hide",
            Self::Hold => "hold",
        }
    }
}

/// The pure outcome of evaluating a text against a resolved policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub mode: ModerationMode,
    /// The matched *policy term* (never user content). `None` on pass/hold.
    pub matched_term: Option<String>,
}

/// Evaluate `text` against an already-resolved policy. Pure: no DB access, no
/// I/O, deterministic. Degraded enforcement short-circuits to Hold before any
/// term is evaluated, so a partial/empty policy can never be read as "safe".
pub fn decide(policy: &EffectiveModerationPolicy, text: &str) -> Decision {
    if policy.enforcement_degraded {
        return Decision {
            mode: ModerationMode::Hold,
            matched_term: None,
        };
    }

    let lower = text.to_lowercase();
    // Pick the strictest match; break ties lexicographically by term so the
    // decision (and the audited term) is deterministic across runs.
    let mut best: Option<(u8, &str, ModerationMode)> = None;
    for term in &policy.terms {
        if term.term.is_empty() {
            continue;
        }
        if !lower.contains(&term.term.to_lowercase()) {
            continue;
        }
        let severity = moderation_mode_severity(&term.mode);
        let mode = ModerationMode::from_policy_mode(&term.mode);
        let strictly_better = best
            .is_none_or(|(bs, bt, _)| severity > bs || (severity == bs && term.term.as_str() < bt));
        if strictly_better {
            best = Some((severity, term.term.as_str(), mode));
        }
    }

    match best {
        Some((_, term, mode)) => Decision {
            mode,
            matched_term: Some(term.to_owned()),
        },
        None => Decision {
            mode: ModerationMode::Pass,
            matched_term: None,
        },
    }
}

/// Opaque, content-free, deterministic message reference. The body feeds a
/// one-way hasher but is never stored: the audit row keeps only this digest
/// and the matched *policy term*. A domain separator keeps it distinct from
/// other hashed identifiers in the system.
pub fn opaque_message_ref(seller_id: i64, bot_id: Option<i64>, text: &str) -> String {
    let mut h = DefaultHasher::new();
    b"MOD".hash(&mut h); // domain separator
    seller_id.hash(&mut h);
    bot_id.hash(&mut h);
    text.hash(&mut h);
    format!("m{:016x}", h.finish())
}

fn reason_code(decision: &Decision) -> &'static str {
    match decision.mode {
        ModerationMode::Hold => "enforcement_degraded",
        ModerationMode::Pass => "no_match",
        _ => "term_match",
    }
}

/// Append one audit row for a decision. `direction` is `inbound` or `outbound`.
/// Returns the row id for tests; failures are non-fatal to the hook.
fn audit(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    direction: &str,
    decision: &Decision,
    message_ref: &str,
) -> Option<i64> {
    let action = format!("{}_{}", direction, decision.mode.action_code());
    db.log_moderation_decision(
        seller_id,
        bot_id,
        decision.matched_term.as_deref(),
        &action,
        Some(reason_code(decision)),
        Some(message_ref),
    )
}

/// Deterministic seller scope for an **inbound** update. A bot attributed to a
/// single seller resolves to that seller's rules; a shared/primary bot (or a
/// sender that is itself a seller) falls back deterministically without ever
/// borrowing another tenant's scope from the message.
pub fn inbound_seller_scope(db: &Db, bot_id: i64, sender_user_id: i64) -> i64 {
    let sellers = db.sellers_for_bot(bot_id);
    if sellers.len() == 1 {
        return sellers[0];
    }
    if db.get_seller(sender_user_id).is_some() {
        return sender_user_id;
    }
    0
}

/// Deterministic seller scope for an **outbound** bot send. Mirrors the
/// inbound resolution from the bot id alone (no sender on a send): sole seller
/// attributed to the bot, else the global namespace.
pub fn outbound_seller_scope(db: &Db, bot_id: i64) -> i64 {
    let sellers = db.sellers_for_bot(bot_id);
    if sellers.len() == 1 { sellers[0] } else { 0 }
}

/// Everything the buyer-facing appeal flow needs from one moderation decision
/// (inbound or outbound). All fields are content-free: the opaque reference
/// and the audit row id only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationEvaluation {
    pub mode: ModerationMode,
    /// The SAME opaque reference stored on the audit row. It is handed to the
    /// "Request review" callback verbatim — never re-derived from the text.
    pub message_ref: String,
    pub decision_id: Option<i64>,
}

/// Inbound hook with the opaque reference exposed for the appeal flow. The
/// decision logic is identical to [`evaluate_inbound`]; this variant just
/// hands back the reference and audit id that were already computed and
/// appended, so a later appeal attaches to the exact same decision.
pub fn evaluate_inbound_detailed(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationEvaluation {
    let policy = db.resolve_effective_policy(seller_id, bot_id);
    let decision = decide(&policy, text);
    let msg_ref = opaque_message_ref(seller_id, bot_id, text);
    let decision_id = audit(db, seller_id, bot_id, "inbound", &decision, &msg_ref);
    if decision.mode == ModerationMode::Hold {
        tracing::warn!(
            seller_id = seller_id,
            bot_id = ?bot_id,
            message_ref = %msg_ref,
            direction = "inbound",
            reason = "enforcement_degraded",
            "moderation enforcement degraded; inbound message held for review"
        );
    }
    ModerationEvaluation {
        mode: decision.mode,
        message_ref: msg_ref,
        decision_id,
    }
}

/// Inbound hook: resolve, decide, audit, and report. Called from the
/// dispatcher before command/text routing. Kept as the thin mode-only view so
/// every existing caller and test stays byte-identical.
pub fn evaluate_inbound(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationMode {
    evaluate_inbound_detailed(db, seller_id, bot_id, text).mode
}

/// Outbound hook with the opaque reference exposed. Identical decision and
/// audit to [`evaluate_outbound`]; the returned reference is the one already
/// written on the outbound audit row, so an outbound hold can offer the same
/// "Request review" action without deriving a new reference from the text.
pub fn evaluate_outbound_detailed(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationEvaluation {
    let policy = db.resolve_effective_policy(seller_id, bot_id);
    let decision = decide(&policy, text);
    let msg_ref = opaque_message_ref(seller_id, bot_id, text);
    let decision_id = audit(db, seller_id, bot_id, "outbound", &decision, &msg_ref);
    if decision.mode == ModerationMode::Hold {
        tracing::warn!(
            seller_id = seller_id,
            bot_id = ?bot_id,
            message_ref = %msg_ref,
            direction = "outbound",
            reason = "enforcement_degraded",
            "moderation enforcement degraded; outbound content held for review"
        );
    }
    ModerationEvaluation {
        mode: decision.mode,
        message_ref: msg_ref,
        decision_id,
    }
}

/// Outbound hook: resolve, decide, audit, and report. Called from the single
/// `ContentSender::send_text` choke point for both bot and owner identities.
pub fn evaluate_outbound(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationMode {
    evaluate_outbound_detailed(db, seller_id, bot_id, text).mode
}

// ---- End-user appeal flow (multibot foundation plan, Step 4.4, L8) ----
//
// This is the ONLY buyer-facing moderation surface. It never reads, logs or
// stores the held message body, a username, or any other content: the inline
// callback carries the *same opaque reference* that the decision audit wrote,
// the reason is a short buyer justification, and the appeal row is written
// through L9's `Db::create_moderation_appeal` with seller/bot scope bound.
// Nothing here exposes content or presence to a web/console path — L7 owns
// the seller/owner review queue, which shows status only.

use teloxide::{
    Bot,
    payloads::SendMessageSetters,
    requests::Requester,
    types::{ChatId, InlineKeyboardButton, InlineKeyboardMarkup},
};

use crate::db::MODERATION_MAX_REASON_LEN;

/// Callback prefix for the single "Request review" action.
pub const APPEAL_CALLBACK_PREFIX: &str = "appeal:";

/// All buyer-facing copy is plain language and discloses no seller, policy,
/// term, mode, or enforcement internals. One uniform message for every
/// blocking reason (hide / review / degraded), so the verdict cannot be
/// inferred from the wording.
pub const APPEAL_OFFER_TEXT: &str =
    "That message didn't go through. You can ask us to take another look.";
pub const APPEAL_PROMPT_TEXT: &str =
    "Tell us briefly why you think this was a mistake (a sentence is enough).";
pub const APPEAL_CONFIRM_TEXT: &str = "Sent for review. We'll let you know.";
pub const APPEAL_DUPLICATE_TEXT: &str = "You've already asked for a review of that message.";
pub const APPEAL_INVALID_TEXT: &str = "Please write a short reason (or /cancel).";
pub const APPEAL_GONE_TEXT: &str = "That review request is no longer available.";

/// How many recent, scoped decision rows are searched when verifying that a
/// callback's opaque reference belongs to a real blocking decision. Bounded on
/// purpose: verification is a read of the append-only audit, not a dump.
const APPEAL_DECISION_LOOKBACK: i64 = 100;

/// A verified, content-free appeal target for one blocking decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppealTarget {
    pub seller_id: i64,
    pub bot_id: i64,
    pub message_ref: String,
    pub decision_id: i64,
}

/// The opaque reference handed to the buyer's "Request review" button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppealOffer {
    pub seller_id: i64,
    pub bot_id: i64,
    pub message_ref: String,
    pub decision_id: Option<i64>,
}

/// Result of a reason submission. `Created` carries the L9 appeal row id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppealOutcome {
    Created(i64),
    AlreadyRequested,
    Invalid,
}

/// `opaque_message_ref` always produces `m` + 16 lowercase hex digits. Accept
/// only that shape, so a forged/oversized callback payload cannot reach the
/// audit lookup or the appeal writer.
fn is_opaque_ref(reference: &str) -> bool {
    reference.len() == 17
        && reference.starts_with('m')
        && reference[1..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Encode the audit's opaque reference into the single callback action.
pub fn appeal_callback_data(reference: &str) -> Option<String> {
    is_opaque_ref(reference).then(|| format!("{APPEAL_CALLBACK_PREFIX}{reference}"))
}

/// Validate + decode a callback payload. The returned string is the exact
/// opaque reference from the original decision audit.
pub fn parse_appeal_callback(data: &str) -> Option<&str> {
    let reference = data.strip_prefix(APPEAL_CALLBACK_PREFIX)?;
    is_opaque_ref(reference).then_some(reference)
}

/// Normalize the buyer's short reason: NUL-filtered, capped at the L9 limit,
/// trimmed. Empty after cleaning is rejected so an appeal always has a reason.
pub fn normalize_appeal_reason(reason: &str) -> Option<String> {
    let cleaned: String = reason
        .chars()
        .filter(|c| *c != '\0')
        .take(MODERATION_MAX_REASON_LEN)
        .collect();
    let cleaned = cleaned.trim().to_owned();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// True for the audit action codes that held content (inbound or outbound).
fn is_blocking_action(action: &str) -> bool {
    matches!(
        action,
        "inbound_hide"
            | "inbound_review"
            | "inbound_hold"
            | "outbound_hide"
            | "outbound_review"
            | "outbound_hold"
    )
}

/// Verify a callback reference against a real, scoped, blocking decision.
/// Both `seller_id` and `bot_id` are bound, so a reference from another tenant
/// or another bot can never be turned into an appeal here. A `pass`/`warn`
/// decision has no appeal affordance and is also rejected.
pub fn find_appeal_target(
    db: &Db,
    seller_id: i64,
    bot_id: i64,
    reference: &str,
) -> Option<AppealTarget> {
    if !is_opaque_ref(reference) {
        return None;
    }
    db.list_moderation_decisions_for_bot(seller_id, bot_id, APPEAL_DECISION_LOOKBACK)
        .into_iter()
        .find(|d| d.message_ref.as_deref() == Some(reference) && is_blocking_action(&d.action))
        .map(|d| AppealTarget {
            seller_id,
            bot_id,
            message_ref: reference.to_owned(),
            decision_id: d.id,
        })
}

/// Idempotency check: any prior appeal for this opaque ref in this exact scope
/// (any status) blocks a second request. No new DB method needed.
pub fn has_appeal_for_ref(db: &Db, seller_id: i64, bot_id: i64, reference: &str) -> bool {
    db.list_moderation_appeals_for_bot(seller_id, bot_id, None)
        .iter()
        .any(|a| a.message_ref == reference)
}

/// Write one appeal through L9's method, deduplicated by opaque reference.
/// Only the scope, the audit's decision id, the opaque ref and the short
/// reason are passed — there is no body, username, or extra field to store.
pub fn submit_appeal(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    decision_id: Option<i64>,
    reference: &str,
    reason: &str,
) -> AppealOutcome {
    if !is_opaque_ref(reference) {
        return AppealOutcome::Invalid;
    }
    let Some(reason) = normalize_appeal_reason(reason) else {
        return AppealOutcome::Invalid;
    };
    let already = match bot_id {
        Some(b) => has_appeal_for_ref(db, seller_id, b, reference),
        None => db
            .list_moderation_appeals(seller_id, None)
            .iter()
            .any(|a| a.message_ref == reference),
    };
    if already {
        return AppealOutcome::AlreadyRequested;
    }
    match db.create_moderation_appeal(seller_id, bot_id, decision_id, reference, Some(&reason)) {
        Some(id) => AppealOutcome::Created(id),
        None => AppealOutcome::Invalid,
    }
}

/// The single inline action offered on a held buyer message.
pub fn appeal_keyboard(reference: &str) -> Option<InlineKeyboardMarkup> {
    Some(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("Request review", appeal_callback_data(reference)?),
    ]]))
}

/// Send one static, content-free line through the central outbound gate. The
/// gate keeps the L10 "every content-bearing send is gated" invariant; if
/// enforcement is degraded or the line is held, the notice is simply omitted
/// (fail closed) — never the held body.
pub async fn send_gated_text(
    bot: &Bot,
    db: &Db,
    chat_id: ChatId,
    seller_id: i64,
    bot_id: i64,
    text: &str,
) -> Result<(), teloxide::RequestError> {
    if gate_outbound(db, seller_id, Some(bot_id), text).is_err() {
        return Ok(());
    }
    bot.send_message(chat_id, text).await?;
    Ok(())
}

/// Send the "Request review" notice for a held buyer message, carrying the
/// original audit reference in exactly one callback button.
pub async fn send_appeal_offer(
    bot: &Bot,
    db: &Db,
    chat_id: ChatId,
    offer: &AppealOffer,
) -> Result<(), teloxide::RequestError> {
    let Some(keyboard) = appeal_keyboard(&offer.message_ref) else {
        return Ok(());
    };
    if gate_outbound(db, offer.seller_id, Some(offer.bot_id), APPEAL_OFFER_TEXT).is_err() {
        return Ok(());
    }
    bot.send_message(chat_id, APPEAL_OFFER_TEXT)
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::EffectiveModerationTerm;

    fn policy(seller_id: i64, terms: &[(&str, &str)], degraded: bool) -> EffectiveModerationPolicy {
        EffectiveModerationPolicy {
            seller_id,
            bot_id: None,
            floor_mode: "warn".to_string(),
            terms: terms
                .iter()
                .map(|(t, m)| EffectiveModerationTerm {
                    term: (*t).to_string(),
                    mode: (*m).to_string(),
                })
                .collect(),
            bot_override_active: false,
            enforcement_degraded: degraded,
        }
    }

    fn test_db(name: &str) -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        let db = Db::new(path.to_str().unwrap()).unwrap();
        (dir, db)
    }

    /// Build a DB whose moderation storage is corrupt but which still opened
    /// cleanly through `init_schema()`. Every moderation object name is
    /// declared in `schema.sql`, so pre-creating a conflicting object before
    /// `Db::new` aborts `init_schema()` *before* the best-effort migration can
    /// register the degraded state. Instead, initialise normally and then
    /// remove the persistent floor table: `load_global_floor_terms` fails and
    /// `resolve_effective_policy` sets `enforcement_degraded` via
    /// `floor_read_failed` — the product's real fail-closed trigger for a
    /// corrupt moderation schema, with no production code touched.
    fn degraded_db(name: &str) -> (tempfile::TempDir, Db) {
        let (dir, db) = test_db(name);
        let path = dir.path().join(name);
        {
            let conn = rusqlite::Connection::open(&path).expect("open test db");
            conn.execute_batch("DROP TABLE IF EXISTS moderation_global_floor;")
                .expect("remove global floor to simulate corrupt moderation schema");
        }
        (dir, db)
    }

    #[test]
    fn decide_maps_all_three_modes_and_pass() {
        assert_eq!(
            decide(&policy(1, &[], false), "clean text").mode,
            ModerationMode::Pass
        );
        assert_eq!(
            decide(&policy(1, &[("bad", "warn")], false), "say bad word").mode,
            ModerationMode::Warn
        );
        assert_eq!(
            decide(&policy(1, &[("bad", "review")], false), "say BAD word").mode,
            ModerationMode::Review
        );
        assert_eq!(
            decide(&policy(1, &[("bad", "hide")], false), "say bad word").mode,
            ModerationMode::Hide
        );
    }

    /// Degraded enforcement holds even when a term WOULD match — a partial
    /// policy must never be trusted as a pass.
    #[test]
    fn decide_degraded_always_holds_and_never_reports_a_term() {
        let d = decide(&policy(1, &[("bad", "hide")], true), "bad word");
        assert_eq!(d.mode, ModerationMode::Hold);
        assert_eq!(d.matched_term, None);
        assert!(d.mode.blocks_inbound_routing());
        assert!(d.mode.blocks_outbound_send());
    }

    #[test]
    fn strictest_matching_term_wins_deterministically() {
        let p = policy(
            1,
            &[("mild", "warn"), ("severe", "hide"), ("other", "review")],
            false,
        );
        assert_eq!(
            decide(&p, "mild and severe here").matched_term.as_deref(),
            Some("severe")
        );
    }

    #[test]
    fn opaque_ref_is_deterministic_and_never_contains_the_body() {
        let r = opaque_message_ref(7, Some(3), "secret body");
        assert_eq!(r, opaque_message_ref(7, Some(3), "secret body"));
        assert_ne!(r, opaque_message_ref(7, Some(3), "different body"));
        assert!(!r.contains("secret"));
        assert!(!r.contains('\0'));
    }

    #[test]
    fn inbound_hide_warn_review_and_degraded_are_distinct() {
        let (_dir, db) = test_db("hook_modes.db");
        let bot = db.create_bot("modes", 0, None, "");
        assert!(db.add_blocked_word(5, "hideword", "hide", true));
        assert!(db.add_blocked_word(5, "warnword", "warn", true));
        assert!(db.add_blocked_word(5, "reviewword", "review", true));
        assert_eq!(
            evaluate_inbound(&db, 5, Some(bot), "a hideword here"),
            ModerationMode::Hide
        );
        assert_eq!(
            evaluate_inbound(&db, 5, Some(bot), "a warnword here"),
            ModerationMode::Warn
        );
        assert_eq!(
            evaluate_inbound(&db, 5, Some(bot), "a reviewword here"),
            ModerationMode::Review
        );
        assert_eq!(
            evaluate_inbound(&db, 5, Some(bot), "nothing here"),
            ModerationMode::Pass
        );

        let rows = db.list_moderation_decisions_for_bot(5, bot, 50);
        assert_eq!(rows.len(), 4);
        let actions: Vec<&str> = rows.iter().map(|r| r.action.as_str()).collect();
        for expected in [
            "inbound_hide",
            "inbound_warn",
            "inbound_review",
            "inbound_pass",
        ] {
            assert!(actions.contains(&expected), "{actions:?}");
        }
    }

    /// Every audit row carries an opaque reference (never the body), the
    /// matched policy term, a short reason, and no content column is added.
    #[test]
    fn audit_row_is_opaque_and_scoped_to_the_tenant() {
        let (_dir, db) = test_db("hook_audit.db");
        assert!(db.add_blocked_word(11, "forbidden", "hide", true));
        let mode = evaluate_inbound(&db, 11, None, "please send forbidden now");
        assert_eq!(mode, ModerationMode::Hide);

        let rows = db.list_moderation_decisions(11, 10);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.term.as_deref(), Some("forbidden"));
        assert_eq!(row.action, "inbound_hide");
        assert_eq!(row.reason.as_deref(), Some("term_match"));
        let r = row.message_ref.as_deref().unwrap_or_default();
        assert!(!r.is_empty() && !r.contains("forbidden") && !r.contains("please"));
        // The other tenant has no audit row: no cross-tenant leakage.
        assert!(db.list_moderation_decisions(12, 10).is_empty());
    }

    /// A term configured for one seller never affects another seller's text.
    #[test]
    fn no_cross_tenant_leakage() {
        let (_dir, db) = test_db("hook_tenancy.db");
        assert!(db.add_blocked_word(101, "secret", "hide", true));
        assert_eq!(
            evaluate_inbound(&db, 101, None, "secret"),
            ModerationMode::Hide
        );
        assert_eq!(
            evaluate_inbound(&db, 202, None, "secret"),
            ModerationMode::Pass
        );
        // A pass is still audited (`inbound_pass`), so zero rows is the wrong
        // expectation. What must never cross tenants is CONTENT: seller 202's
        // rows carry only an opaque ref, never seller 101's matched term or
        // the message body.
        let other_rows = db.list_moderation_decisions(202, 10);
        assert!(!other_rows.is_empty(), "a pass is still audited");
        for row in &other_rows {
            assert_eq!(row.seller_id, 202);
            assert_eq!(row.action, "inbound_pass");
            assert_eq!(row.term, None, "no seller-101 policy term leaked");
            let r = row.message_ref.as_deref().unwrap_or_default();
            assert!(!r.is_empty());
            assert!(!r.contains("secret"), "no message body leaked");
            assert_eq!(r, opaque_message_ref(202, None, "secret"));
        }
        // The owning tenant's row, by contrast, does record the matched term.
        let own_rows = db.list_moderation_decisions(101, 10);
        assert!(own_rows.iter().any(|r| r.term.as_deref() == Some("secret")));
    }

    /// Hooks read policy only: evaluating must not mutate the owner floor.
    #[test]
    fn hooks_never_write_the_global_floor() {
        let (_dir, db) = test_db("hook_floor_readonly.db");
        let before = db.list_moderation_global_floor().len();
        let _ = evaluate_inbound(&db, 1, None, "anything");
        let _ = evaluate_outbound(&db, 1, None, "anything");
        assert_eq!(db.list_moderation_global_floor().len(), before);
    }

    /// Full degraded path on a database whose moderation storage is corrupt:
    /// resolution is degraded, so the hook holds.
    #[test]
    fn degraded_storage_holds_the_whole_hook() {
        let (_dir, db) = degraded_db("hook_degraded.db");
        assert!(db.resolve_effective_policy(1, None).enforcement_degraded);
        assert_eq!(
            evaluate_inbound(&db, 1, None, "anything"),
            ModerationMode::Hold
        );
        assert_eq!(
            evaluate_outbound(&db, 1, None, "anything"),
            ModerationMode::Hold
        );
    }

    /// The central pre-transport gate returns the opaque hold marker (never
    /// the body) for a matching term, and passes clean content unchanged.
    #[test]
    fn gate_outbound_holds_matching_term_and_passes_clean_text() {
        let (_dir, db) = test_db("gate_outbound.db");
        assert!(db.add_blocked_word(9, "forbidden", "hide", true));
        let held = gate_outbound(&db, 9, Some(4), "a forbidden draft").unwrap_err();
        assert_eq!(held, OutboundHold);
        assert!(held.to_string().contains("moderation_hold"));
        assert!(!held.to_string().contains("forbidden"));
        assert!(gate_outbound(&db, 9, Some(4), "clean draft").is_ok());
    }

    /// Seller scope is honored by the gate: a term configured for seller A
    /// holds only that seller's content, not seller B's.
    #[test]
    fn gate_outbound_honors_seller_scope() {
        let (_dir, db) = test_db("gate_outbound_scope.db");
        assert!(db.add_blocked_word(77, "secretterm", "review", true));
        assert!(gate_outbound(&db, 77, None, "secretterm here").is_err());
        assert!(gate_outbound(&db, 78, None, "secretterm here").is_ok());
    }

    /// Degraded enforcement holds through the gate too — no term needed.
    #[test]
    fn gate_outbound_holds_when_enforcement_is_degraded() {
        let (_dir, db) = degraded_db("gate_degraded.db");
        assert!(db.resolve_effective_policy(1, None).enforcement_degraded);
        assert!(gate_outbound(&db, 1, None, "anything clean").is_err());
    }

    // ---- End-user appeal flow (Step 4.4) ----

    /// A decision is evaluated with a blocked term, then appealed. Returns the
    /// temp dir (kept alive), db, bot id and the EXACT opaque audit reference.
    fn appeal_fixture(name: &str, seller: i64) -> (tempfile::TempDir, Db, i64, String) {
        let (dir, db) = test_db(name);
        let bot = db.create_bot("appealbot", seller, None, "");
        assert!(db.add_blocked_word(seller, "blockedterm", "hide", true));
        let evaluation =
            evaluate_inbound_detailed(&db, seller, Some(bot), "please send blockedterm now");
        assert_eq!(evaluation.mode, ModerationMode::Hide);
        (dir, db, bot, evaluation.message_ref)
    }

    #[test]
    fn appeal_created_with_opaque_ref_reason_and_pending_status() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_create.db", 21);
        let target = find_appeal_target(&db, 21, bot, &reference).unwrap();
        assert_eq!(target.seller_id, 21);
        assert_eq!(target.bot_id, bot);
        assert_eq!(target.message_ref, reference);
        let created = submit_appeal(
            &db,
            21,
            Some(bot),
            Some(target.decision_id),
            &reference,
            "I think this was a mistake",
        );
        let id = match created {
            AppealOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let row = db.get_moderation_appeal(21, id).unwrap();
        assert_eq!(row.seller_id, 21);
        assert_eq!(row.bot_id, Some(bot));
        assert_eq!(row.decision_id, Some(target.decision_id));
        assert_eq!(row.message_ref, reference);
        assert_eq!(row.reason.as_deref(), Some("I think this was a mistake"));
        assert_eq!(row.status, "pending");
    }

    #[test]
    fn appeal_and_target_are_scoped_by_seller_and_bot() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_scope.db", 31);
        let other_bot = db.create_bot("other", 31, None, "");
        assert!(matches!(
            submit_appeal(&db, 31, Some(bot), None, &reference, "reason"),
            AppealOutcome::Created(_)
        ));
        // A different seller sees no appeal and cannot verify this ref.
        assert!(db.list_moderation_appeals(32, None).is_empty());
        assert!(find_appeal_target(&db, 32, bot, &reference).is_none());
        // A different bot under the SAME seller has a disjoint queue/target.
        assert!(
            db.list_moderation_appeals_for_bot(31, other_bot, None)
                .is_empty()
        );
        assert!(find_appeal_target(&db, 31, other_bot, &reference).is_none());
    }

    #[test]
    fn duplicate_appeal_for_same_ref_is_rejected() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_dup.db", 41);
        assert!(matches!(
            submit_appeal(&db, 41, Some(bot), None, &reference, "first reason"),
            AppealOutcome::Created(_)
        ));
        assert_eq!(
            submit_appeal(&db, 41, Some(bot), None, &reference, "second reason"),
            AppealOutcome::AlreadyRequested
        );
        assert_eq!(db.list_moderation_appeals_for_bot(41, bot, None).len(), 1);
        assert_eq!(
            db.list_moderation_appeals(41, None)[0].reason.as_deref(),
            Some("first reason")
        );
    }

    #[test]
    fn appeal_stores_no_message_body_or_username() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_body.db", 51);
        // "blockedterm" is the held body; it must never appear in the appeal.
        assert!(matches!(
            submit_appeal(&db, 51, Some(bot), None, &reference, "please reconsider"),
            AppealOutcome::Created(_)
        ));
        let row = &db.list_moderation_appeals_for_bot(51, bot, None)[0];
        assert_eq!(
            row.message_ref,
            opaque_message_ref(51, Some(bot), "please send blockedterm now")
        );
        assert!(!row.message_ref.contains("blockedterm"));
        assert!(!row.message_ref.contains("please"));
        assert_eq!(row.reason.as_deref(), Some("please reconsider"));
        // The appeal record has no body/username field: the only free text is
        // the buyer's short justification above. No user id is a parameter.
    }

    #[test]
    fn appeal_reason_is_capped_and_nul_filtered() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_reason.db", 61);
        let mut reason = String::from("abc\0def");
        reason.push_str(&"x".repeat(MODERATION_MAX_REASON_LEN + 50));
        assert!(matches!(
            submit_appeal(&db, 61, Some(bot), None, &reference, &reason),
            AppealOutcome::Created(_)
        ));
        let stored = db.list_moderation_appeals_for_bot(61, bot, None)[0]
            .reason
            .clone()
            .unwrap();
        assert!(!stored.contains('\0'));
        assert!(stored.chars().count() <= MODERATION_MAX_REASON_LEN);
        assert!(stored.starts_with("abcdef"));
        // NUL-only (or empty) reasons are refused.
        assert_eq!(
            submit_appeal(&db, 61, Some(bot), None, &reference, "\0\0"),
            AppealOutcome::Invalid
        );
    }

    #[test]
    fn appeal_target_requires_a_real_blocking_decision() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_target.db", 71);
        assert!(find_appeal_target(&db, 71, bot, &reference).is_some());
        // A well-shaped but unissued reference, and malformed data, are refused.
        assert!(find_appeal_target(&db, 71, bot, "m0000000000000000").is_none());
        assert!(find_appeal_target(&db, 71, bot, "not-an-opaque-ref").is_none());
        // Issuance is validated at the callback boundary via
        // `find_appeal_target` (asserted above), not in `submit_appeal`. The
        // writer only checks the opaque REF SHAPE and the reason, so a
        // well-shaped but unissued ref is accepted here.
        assert!(matches!(
            submit_appeal(&db, 71, Some(bot), None, "m0000000000000000", "r"),
            AppealOutcome::Created(_)
        ));
        // A pass decision never gets an appeal affordance.
        let passing = evaluate_inbound_detailed(&db, 71, Some(bot), "something clean");
        assert_eq!(passing.mode, ModerationMode::Pass);
        assert!(find_appeal_target(&db, 71, bot, &passing.message_ref).is_none());
    }

    /// An outbound hold is audited with an opaque ref that can feed the SAME
    /// appeal flow (via `find_appeal_target` → `submit_appeal`) without ever
    /// re-deriving a reference from the held text. No floor write occurs.
    #[test]
    fn outbound_hold_ref_can_feed_the_appeal_flow() {
        let (_dir, db, bot, _inbound_ref) = appeal_fixture("appeal_outbound.db", 91);
        let before = db.list_moderation_global_floor().len();
        let outbound =
            evaluate_outbound_detailed(&db, 91, Some(bot), "a blockedterm outbound draft");
        assert_eq!(outbound.mode, ModerationMode::Hide);
        assert!(outbound.mode.blocks_outbound_send());
        // The outbound decision's audit ref is a valid appeal target.
        let target = find_appeal_target(&db, 91, bot, &outbound.message_ref).unwrap();
        assert_eq!(target.decision_id, outbound.decision_id.unwrap());
        let row_id = match submit_appeal(
            &db,
            91,
            Some(bot),
            Some(target.decision_id),
            &outbound.message_ref,
            "please review",
        ) {
            AppealOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        };
        let row = db.get_moderation_appeal(91, row_id).unwrap();
        assert_eq!(row.message_ref, outbound.message_ref);
        assert!(!row.message_ref.contains("blockedterm"));
        assert_eq!(row.status, "pending");
        assert_eq!(db.list_moderation_global_floor().len(), before);
    }

    #[test]
    fn appeal_callback_carries_only_the_opaque_ref() {
        let reference = opaque_message_ref(7, Some(3), "body text");
        let data = appeal_callback_data(&reference).unwrap();
        assert_eq!(data, format!("appeal:{reference}"));
        assert_eq!(parse_appeal_callback(&data), Some(reference.as_str()));
        assert_eq!(parse_appeal_callback("menu:shop"), None);
        assert_eq!(parse_appeal_callback("appeal:m000000000000000Z"), None);
        assert_eq!(
            parse_appeal_callback(&format!("appeal:{}", "x".repeat(300))),
            None
        );
        assert!(appeal_keyboard(&reference).is_some());
        assert!(appeal_keyboard("bad").is_none());
        assert!(!data.contains("body"));
    }

    #[test]
    fn appeal_flow_never_writes_floor_and_degraded_path_still_holds() {
        let (_dir, db, bot, reference) = appeal_fixture("appeal_floor.db", 81);
        let before = db.list_moderation_global_floor().len();
        assert!(matches!(
            submit_appeal(&db, 81, Some(bot), None, &reference, "reason"),
            AppealOutcome::Created(_)
        ));
        assert_eq!(db.list_moderation_global_floor().len(), before);

        // Degraded enforcement still holds the message; the audited reference
        // is opaque and no floor term was added by the appeal path.
        let (_dir, degraded) = degraded_db("appeal_degraded.db");
        let evaluation = evaluate_inbound_detailed(&degraded, 1, None, "anything clean");
        assert_eq!(evaluation.mode, ModerationMode::Hold);
        assert!(evaluation.mode.blocks_inbound_routing());
        let rows = degraded.list_moderation_decisions(1, 10);
        assert!(!rows.is_empty());
        assert_eq!(
            rows[0].message_ref.as_deref(),
            Some(evaluation.message_ref.as_str())
        );
        assert!(!rows[0].message_ref.as_ref().unwrap().contains("anything"));
    }
}
