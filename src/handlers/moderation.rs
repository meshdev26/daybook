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

/// Inbound hook: resolve, decide, audit, and report. Called from the
/// dispatcher before command/text routing.
pub fn evaluate_inbound(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationMode {
    let policy = db.resolve_effective_policy(seller_id, bot_id);
    let decision = decide(&policy, text);
    let msg_ref = opaque_message_ref(seller_id, bot_id, text);
    audit(db, seller_id, bot_id, "inbound", &decision, &msg_ref);
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
    decision.mode
}

/// Outbound hook: resolve, decide, audit, and report. Called from the single
/// `ContentSender::send_text` choke point for both bot and owner identities.
pub fn evaluate_outbound(
    db: &Db,
    seller_id: i64,
    bot_id: Option<i64>,
    text: &str,
) -> ModerationMode {
    let policy = db.resolve_effective_policy(seller_id, bot_id);
    let decision = decide(&policy, text);
    let msg_ref = opaque_message_ref(seller_id, bot_id, text);
    audit(db, seller_id, bot_id, "outbound", &decision, &msg_ref);
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
    decision.mode
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
        assert!(db.list_moderation_decisions(202, 10).is_empty());
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

    /// Full degraded path on a database whose moderation DDL was broken
    /// before `Db::new`: resolution is degraded, so the hook holds.
    #[test]
    fn degraded_storage_holds_the_whole_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook_degraded.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE idx_mod_blocked_seller(x INTEGER);")
                .unwrap();
        }
        let db = Db::new(path.to_str().unwrap()).unwrap();
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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gate_degraded.db");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE idx_mod_blocked_seller(x INTEGER);")
                .unwrap();
        }
        let db = Db::new(path.to_str().unwrap()).unwrap();
        assert!(gate_outbound(&db, 1, None, "anything clean").is_err());
    }
}
