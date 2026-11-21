//! Shared money-safety primitives (MONEY_SAFETY_SPEC decisions 3/4/5).
//!
//! This module is the SINGLE source of truth for:
//!   * the fail-closed transfer error classifier (§4.2) — REST, bot and web
//!     must all branch on the same enum, never on a boolean with an inverted
//!     default;
//!   * the persist-then-call transfer helper (§4.1) — no Crypto Pay transfer
//!     may be issued outside it, so a spend_id is always recorded first;
//!   * pure clock/cutoff helpers (§3/§5) so the sweepers in `main.rs` can be
//!     tested at 30s/24h/72h and 30min/24h boundaries without sleeping.
//!
//! No money semantics live here beyond the spec's; no secret or message
//! content is ever logged or placed in a digest.

use chrono::{DateTime, Timelike, Utc};
use serde_json::Value;

/// §4.2: the two outcomes of a Crypto Pay transfer attempt. `DefiniteRejected`
/// means the API positively received and refused the request (no money moved);
/// `Ambiguous` means the money state is unknown and the row must stay
/// `executing` for operator reconciliation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferOutcome {
    DefiniteRejected,
    Ambiguous,
}

/// Fail-closed classifier: ONLY positively recognized API refusals are
/// `DefiniteRejected`. Everything else — 5xx, 408, timeouts, connect/reset/
/// broken pipe, decode/build errors, empty strings and any unrecognized or
/// future error shape — is `Ambiguous`. A novel error must never be treated
/// as definite, because reverting an executed transfer causes a double payout.
pub fn classify_transfer_error(err: &str) -> TransferOutcome {
    let e = err.trim();
    if e.is_empty() {
        return TransferOutcome::Ambiguous;
    }

    // `CryptoPay::call` renders non-2xx responses as "HTTP {status} - {body}";
    // the status token is authoritative. Only 4xx other than 408 is a
    // definite server-side refusal. 408/5xx are unknowable (the API may have
    // committed before the response failed).
    if let Some(rest) = e.strip_prefix("HTTP ") {
        let token: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(code) = token.parse::<u16>() {
            return if (400..=499).contains(&code) && code != 408 {
                TransferOutcome::DefiniteRejected
            } else {
                TransferOutcome::Ambiguous
            };
        }
        return TransferOutcome::Ambiguous;
    }

    // `ok:false` business rejections arrive from crypto.rs as the bare error
    // string. Recognize a conservative allowlist of documented refusals;
    // normalize spaces/hyphens so "insufficient funds" and
    // "INSUFFICIENT_FUNDS" classify identically. An unknown token falls
    // through to Ambiguous — that is the whole point of the shared classifier.
    let normalized = e.to_ascii_uppercase().replace([' ', '-'], "_");
    const DEFINITE_TOKENS: &[&str] = &[
        "TRANSFER_FAILED",
        "INSUFFICIENT_FUNDS",
        "NOT_ENOUGH_FUNDS",
        "INSUFFICIENT_BALANCE",
        "BALANCE_NOT_ENOUGH",
        "NOT_ENOUGH_COINS",
        "INVALID_USER_ID",
        "USER_NOT_FOUND",
        "INVALID_ASSET",
        "ASSET_NOT_SUPPORTED",
        "AMOUNT_TOO_SMALL",
        "AMOUNT_TOO_LARGE",
        "WITHDRAWAL_FAILED",
        // crypto.rs substitutes this when ok:false carries no `error` field:
        // still a parsed business refusal, so definite.
        "UNKNOWN_ERROR",
    ];
    let definite = DEFINITE_TOKENS.iter().any(|token| {
        normalized == *token
            || normalized.starts_with(&format!("{token}_"))
            || normalized.starts_with(&format!("{token}:"))
            || normalized.starts_with(&format!("{token}("))
    });
    if definite {
        return TransferOutcome::DefiniteRejected;
    }

    TransferOutcome::Ambiguous
}

/// Outcome of a guarded transfer. Each variant carries the spend_id that was
/// persisted before the call so callers can audit the exact attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardedTransferOutcome {
    Succeeded { spend_id: String, receipt: Value },
    DefiniteRejected { spend_id: String, error: String },
    Ambiguous { spend_id: String, error: String },
}

/// Mint a CSPRNG-backed unique key prefix. Entropy failure is a hard error so
/// no request path silently falls back to a predictable key.
fn fresh_key_nonce() -> Result<String, String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| format!("OS entropy unavailable: {e}"))?;
    let mut nonce = String::with_capacity(buf.len() * 2);
    for b in buf {
        use std::fmt::Write;
        let _ = write!(&mut nonce, "{b:02x}");
    }
    Ok(nonce)
}

/// Mint a fresh refund idempotency key for a client that did not supply one.
pub fn fresh_idem_key(prefix: &str) -> Result<String, String> {
    Ok(format!("{prefix}:{}", fresh_key_nonce()?))
}

/// Mint a globally unique spend_id. The CSPRNG nonce makes it globally unique;
/// the row_type/row_id prefix makes every attempt deterministically
/// attributable to its money row. `getrandom::fill` failure is a hard error —
/// no transfer is attempted without a persisted id.
fn fresh_spend_id(row_type: &str, row_id: i64) -> Result<String, String> {
    Ok(format!("{row_type}-{row_id}-{}", fresh_key_nonce()?))
}

/// Execute a transfer with the §4.1 discipline: persist the attempt BEFORE
/// the call, pass the persisted spend_id to the transport, and audit the
/// outcome. `reuse_spend_id = Some(sid)` reuses an already-persisted attempt
/// (ambiguous reconciliation); `None` mints a fresh attempt/spend_id (first
/// try or a definite-rejected retry). This helper is the ONLY sanctioned
/// place to invoke a Crypto Pay transfer from REST/bot logic.
pub async fn execute_guarded_transfer<F, Fut>(
    db: &crate::db::Db,
    row_type: &str,
    row_id: i64,
    seller_id: i64,
    reuse_spend_id: Option<&str>,
    transfer: F,
) -> Result<GuardedTransferOutcome, String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let spend_id = match reuse_spend_id {
        Some(existing) => {
            let attempt = db
                .get_transfer_attempt(existing)
                .ok_or_else(|| "spend_id has no persisted transfer attempt".to_string())?;
            if attempt.row_type != row_type || attempt.row_id != row_id {
                return Err("spend_id belongs to a different money row".to_string());
            }
            // Reuse is only for an unresolved/ambiguous attempt. A recorded
            // success or a definitive rejection must never be replayed with
            // the same key (that is a double-payout / stale-retry hazard).
            if !matches!(attempt.outcome.as_deref(), None | Some("ambiguous")) {
                return Err("spend_id was already resolved".to_string());
            }
            existing.to_string()
        }
        None => {
            let fresh = fresh_spend_id(row_type, row_id)?;
            db.record_transfer_attempt(row_type, row_id, seller_id, &fresh)
                .map_err(|e| format!("failed to persist transfer attempt: {e}"))?;
            fresh
        }
    };

    match transfer(spend_id.clone()).await {
        Ok(receipt) => {
            db.set_transfer_attempt_outcome(&spend_id, "executed");
            Ok(GuardedTransferOutcome::Succeeded { spend_id, receipt })
        }
        Err(e) => {
            let outcome = classify_transfer_error(&e);
            let token = match outcome {
                TransferOutcome::DefiniteRejected => "definite_rejected",
                TransferOutcome::Ambiguous => "ambiguous",
            };
            db.set_transfer_attempt_outcome(&spend_id, token);
            Ok(match outcome {
                TransferOutcome::DefiniteRejected => {
                    GuardedTransferOutcome::DefiniteRejected { spend_id, error: e }
                }
                TransferOutcome::Ambiguous => {
                    GuardedTransferOutcome::Ambiguous { spend_id, error: e }
                }
            })
        }
    }
}

// ---- Injected-clock helpers (§3 / §5) ----

/// Cutoff for the 30-second `paid_ungranted` grant retry (§3.3).
pub fn paid_ungranted_retry_cutoff(now: DateTime<Utc>) -> String {
    (now - chrono::Duration::try_seconds(crate::db::PAID_UNGRANTED_RETRY_SECONDS)
        .unwrap_or_default())
    .to_rfc3339()
}

/// Cutoff for the 24-hour auto-refund trigger (§3.4).
pub fn paid_ungranted_auto_refund_cutoff(now: DateTime<Utc>) -> String {
    (now - chrono::Duration::try_hours(crate::db::PAID_UNGRANTED_AUTO_REFUND_HOURS)
        .unwrap_or_default())
    .to_rfc3339()
}

/// Cutoff for the 72-hour blocking escalation / payout hold (§3.5).
pub fn paid_ungranted_escalation_cutoff(now: DateTime<Utc>) -> String {
    (now - chrono::Duration::try_hours(crate::db::PAID_UNGRANTED_ESCALATION_HOURS)
        .unwrap_or_default())
    .to_rfc3339()
}

/// §5 cutoffs: (executing-due = now-30min, reconciliation-overdue = now-24h).
pub fn reconciliation_cutoffs(now: DateTime<Utc>) -> (String, String) {
    let due = (now
        - chrono::Duration::try_minutes(crate::db::RECONCILIATION_DUE_MINUTES).unwrap_or_default())
    .to_rfc3339();
    let overdue = (now
        - chrono::Duration::try_hours(crate::db::RECONCILIATION_OVERDUE_HOURS).unwrap_or_default())
    .to_rfc3339();
    (due, overdue)
}

/// The §5.4 daily digest runs once per day at 09:00 UTC. The sweeper ticks
/// every minute, so the 09:00 minute is the single firing window.
pub fn should_run_digest(now: DateTime<Utc>) -> bool {
    now.hour() == 9 && now.minute() == 0
}

/// Complete owner view: core's `reconciliation_rows` returns overdue-relevant
/// rows by injected cutoff; this also folds in every `needs_reconciliation`
/// refund/payout still inside its 24h window so the owner list and the daily
/// digest never hide a row simply because it is young. Dedup is by
/// `(row_type, id)`; no money state is changed here.
pub fn all_reconciliation_rows(
    db: &crate::db::Db,
    due: &str,
    overdue: &str,
) -> Vec<crate::db::ReconciliationRow> {
    let mut rows = db.reconciliation_rows(due, overdue);
    let mut seen: std::collections::HashSet<(String, i64)> =
        rows.iter().map(|r| (r.row_type.clone(), r.id)).collect();
    for r in db.list_refunds(None) {
        if r.7 == "needs_reconciliation" && seen.insert(("refund".to_string(), r.0)) {
            rows.push(crate::db::ReconciliationRow {
                row_type: "refund".to_string(),
                id: r.0,
                seller_id: r.1,
                amount: r.4,
                asset: r.5,
                status: r.7,
                since: String::new(),
                spend_id: db
                    .latest_transfer_attempt("refund", r.0)
                    .map(|a| a.spend_id),
                overdue: false,
            });
        }
    }
    for p in db.list_payouts(None) {
        if p.5 == "needs_reconciliation" && seen.insert(("payout".to_string(), p.0)) {
            rows.push(crate::db::ReconciliationRow {
                row_type: "payout".to_string(),
                id: p.0,
                seller_id: p.1,
                amount: p.2,
                asset: p.3,
                status: p.5,
                since: String::new(),
                spend_id: db
                    .latest_transfer_attempt("payout", p.0)
                    .map(|a| a.spend_id),
                overdue: false,
            });
        }
    }
    rows
}

/// Stable SHA-256 over the sorted (row_type:id) list in a digest. Included in
/// the audit metadata so a reviewer can prove which rows the digest covered.
pub fn digest_row_ids_hash(rows: &[crate::db::ReconciliationRow]) -> String {
    use sha2::Digest;
    let mut ids: Vec<String> = rows
        .iter()
        .map(|r| format!("{}:{}", r.row_type, r.id))
        .collect();
    ids.sort();
    let mut hasher = sha2::Sha256::new();
    hasher.update(ids.join(",").as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Human text for the owner digest. Includes only ids, amounts, sellers,
/// ages and spend_ids — never message content or credentials.
pub fn format_digest(rows: &[crate::db::ReconciliationRow], now: DateTime<Utc>) -> String {
    let mut out = String::from("Money reconciliation digest (09:00 UTC):\n");
    if rows.is_empty() {
        out.push_str("No executing or reconciling transfers.\n");
        return out;
    }
    for r in rows {
        let age_hours = (now
            - DateTime::parse_from_rfc3339(&r.since)
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(now))
        .num_hours();
        out.push_str(&format!(
            "- {} #{} | seller {} | {} {} | status {} | age {}h | spend_id {}\n",
            r.row_type,
            r.id,
            r.seller_id,
            r.amount,
            r.asset,
            r.status,
            age_hours,
            r.spend_id.as_deref().unwrap_or("-")
        ));
        if r.overdue {
            out.push_str("  BLOCKING: past the 24h owner-decision deadline.\n");
        }
    }
    out
}

/// Parse a base-10 money amount into integer minor units (scale 8). Mirrors
/// the data-layer quantization used by the cumulative cap; no `f64` is used
/// for money. Invalid/negative legacy values return None (fail closed).
fn minor_units(value: &str) -> Option<i128> {
    const SCALE: u32 = 8;
    let s = value.trim();
    if s.is_empty() || s.starts_with('-') || s.starts_with('+') {
        return None;
    }
    let mut parts = s.splitn(2, '.');
    let whole: i128 = parts.next()?.parse().ok()?;
    let frac = parts.next().unwrap_or("");
    if frac.len() > SCALE as usize || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut frac_units: i128 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    for _ in frac.len()..SCALE as usize {
        frac_units = frac_units.checked_mul(10)?;
    }
    whole
        .checked_mul(10_i128.pow(SCALE))?
        .checked_add(frac_units)
}

/// §1.3: a full refund (counted refunds reach the invoiced amount, exact
/// minor-unit equality) flips the invoice to `refunded` and revokes the
/// grant; a partial refund leaves it `paid`. Read-only decision helper.
pub fn is_full_refund(db: &crate::db::Db, invoice_id: &str) -> bool {
    let Some(inv) = db.get_invoice(invoice_id) else {
        return false;
    };
    let Some(invoice_minor) = minor_units(&inv.0) else {
        return false;
    };
    let mut counted: i128 = 0;
    for r in db.list_refunds(None) {
        if r.2.as_deref() == Some(invoice_id) && r.7 != "void" {
            let Some(units) = minor_units(&r.4) else {
                return false;
            };
            let Some(next) = counted.checked_add(units) else {
                return false;
            };
            counted = next;
        }
    }
    counted >= invoice_minor
}

/// Whether an audited event of this kind already exists for a money row.
/// Used by the sweepers to raise a blocking alert once, not every tick.
pub fn has_audit_action(
    db: &crate::db::Db,
    action: &str,
    target_type: &str,
    target_id: &str,
) -> bool {
    db.list_audit_log(100_000).into_iter().any(|row| {
        let (_, _, _, ref a, ref tt, ref tid, _, _) = row;
        a == action && tt == target_type && tid.as_deref() == Some(target_id)
    })
}

/// Sentinel written by legacy `create_refund` rows before the creator columns
/// existed (core migration, `db.rs`). Such rows carry no attributable human in
/// the persisted columns, so — and only so — do we consult the audit log.
const LEGACY_CREATOR_ID: i64 = -1;
const LEGACY_CREATOR_ROLE: &str = "unknown";

/// Resolve a refund's human creator (§2 separation of duties).
///
/// The authoritative source is the persisted `created_by`/`created_role`
/// columns written by `Db::create_refund_guarded` (core). Unlike a `refund_create`
/// audit event these never age out of the audit window on a mature database,
/// so a creator cannot escape the separation check simply by waiting long
/// enough. AUDIT FALLBACK is deliberately narrow: it applies only to legacy
/// rows whose persisted creator is the `-1` / `"unknown"` sentinel (created
/// before the columns existed, or never backfilled because they had no audit
/// event). A missing refund row returns `None` — there is no row to gate.
/// System-created refunds carry the sentinel and are exempt from the creator
/// check (they still require an Owner approver).
pub fn refund_creator(db: &crate::db::Db, id: i64) -> Option<(i64, String)> {
    match db.refund_creator(id) {
        Some((creator, role)) if creator != LEGACY_CREATOR_ID && role != LEGACY_CREATOR_ROLE => {
            Some((creator, role))
        }
        Some(_) => refund_creator_from_audit(db, id),
        None => None,
    }
}

/// Narrow legacy-only fallback: read the oldest `refund_create` audit event
/// for the row. It is the creation event, never a later approver. Once a
/// legacy row ages out of the audit window it can no longer be attributed and
/// this returns `None` (fail closed — no guessed creator).
fn refund_creator_from_audit(db: &crate::db::Db, id: i64) -> Option<(i64, String)> {
    let target = id.to_string();
    db.list_audit_log(100_000)
        .into_iter()
        .rev() // oldest first — the create event, never a later approver
        .find(|(_, _, _, action, target_type, target_id, _, _)| {
            action == "refund_create"
                && target_type == "refund"
                && target_id.as_deref() == Some(target.as_str())
        })
        .map(|(_, actor_id, actor_role, _, _, _, _, _)| (actor_id, actor_role))
}

/// §2 separation of duties, shared by the REST and web surfaces: `true` when
/// `account_id` is the human that created `refund_id`. Both the REST
/// reconciliation-resolve gate and the web `creator_owns_refund` gate call
/// this single helper so the two paths cannot drift. System-created refunds
/// have no attributable creator and therefore return `false` (they are still
/// Owner-gated by the callers).
pub fn creator_owns_refund(db: &crate::db::Db, account_id: i64, refund_id: i64) -> bool {
    refund_creator(db, refund_id).is_some_and(|(creator, _)| creator == account_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_recognized_definites() {
        for e in [
            "TRANSFER_FAILED",
            "insufficient funds",
            "INSUFFICIENT_FUNDS",
            "NOT_ENOUGH_FUNDS: balance",
            "invalid user id",
            "WITHDRAWAL_FAILED(retry later)",
            "unknown error",
            "HTTP 400 - {\"error\":\"BAD_REQUEST\"}",
            "HTTP 403 - forbidden",
            "HTTP 404 - not found",
            "HTTP 422 - invalid",
        ] {
            assert_eq!(
                classify_transfer_error(e),
                TransferOutcome::DefiniteRejected,
                "expected definite for {e:?}"
            );
        }
    }

    #[test]
    fn classifier_ambiguous_including_unrecognized() {
        for e in [
            "",
            "   ",
            "HTTP 500 - oops",
            "HTTP 503 - unavailable",
            "HTTP 408 - timeout",
            "error sending request for url (http://x): operation timed out",
            "error trying to connect: connection reset by peer",
            "broken pipe",
            "expected value at line 1 column 1",
            "JSON decode failure: <html>",
            "some brand new error we have never seen",
            "weird \u{1f4a9} glyphs",
            "HTTP not-a-number - body",
        ] {
            assert_eq!(
                classify_transfer_error(e),
                TransferOutcome::Ambiguous,
                "expected ambiguous for {e:?}"
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn classifier_any_unrecognized_string_is_ambiguous(s in ".*") {
            // Known definite tokens are excluded; everything else must be
            // Ambiguous (fail-closed).
            let upper = s.to_ascii_uppercase();
            let known = upper == "TRANSFER_FAILED"
                || upper == "UNKNOWN ERROR"
                || upper == "UNKNOWN_ERROR"
                || upper == "INSUFFICIENT FUNDS";
            if !known {
                proptest::prop_assert_eq!(
                    classify_transfer_error(&s),
                    TransferOutcome::Ambiguous,
                    "unrecognized error must be ambiguous: {:?}", s
                );
            }
        }
    }

    #[test]
    fn injected_clock_cutoffs_match_spec_constants() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-20T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            paid_ungranted_retry_cutoff(now),
            "2026-11-20T11:59:30+00:00"
        );
        assert_eq!(
            paid_ungranted_auto_refund_cutoff(now),
            "2026-11-19T12:00:00+00:00"
        );
        assert_eq!(
            paid_ungranted_escalation_cutoff(now),
            "2026-11-17T12:00:00+00:00"
        );
        assert_eq!(
            reconciliation_cutoffs(now),
            (
                "2026-11-20T11:30:00+00:00".to_string(),
                "2026-11-19T12:00:00+00:00".to_string()
            )
        );
        assert!(should_run_digest(
            chrono::DateTime::parse_from_rfc3339("2026-11-20T09:00:00Z")
                .unwrap()
                .with_timezone(&Utc)
        ));
        assert!(!should_run_digest(
            chrono::DateTime::parse_from_rfc3339("2026-11-20T09:01:00Z")
                .unwrap()
                .with_timezone(&Utc)
        ));
    }

    fn test_db(name: &str) -> (std::sync::Arc<crate::db::Db>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{name}.db"));
        let db = std::sync::Arc::new(crate::db::Db::new(path.to_str().unwrap()).unwrap());
        (db, dir)
    }

    #[tokio::test]
    async fn guarded_transfer_persists_spend_id_before_call() {
        let (db, _dir) = test_db("money_guarded_before");
        let db2 = db.clone();
        let outcome = execute_guarded_transfer(&db, "refund", 7, 42, None, |spend_id| async move {
            // The id MUST already be durable when the transport is called.
            assert!(db2.get_transfer_attempt(&spend_id).is_some());
            Ok(serde_json::json!({"transfer_id": 1}))
        })
        .await
        .unwrap();
        let GuardedTransferOutcome::Succeeded { spend_id, .. } = outcome else {
            panic!("expected success");
        };
        let attempt = db.get_transfer_attempt(&spend_id).unwrap();
        assert_eq!(attempt.attempt_no, 1);
        assert_eq!(attempt.outcome.as_deref(), Some("executed"));
        assert!(spend_id.starts_with("refund-7-"));
    }

    #[tokio::test]
    async fn guarded_transfer_ambiguous_reuses_spend_id() {
        let (db, _dir) = test_db("money_guarded_ambiguous");
        let first = execute_guarded_transfer(&db, "payout", 3, 42, None, |_| async {
            Err("operation timed out".to_string())
        })
        .await
        .unwrap();
        let GuardedTransferOutcome::Ambiguous { spend_id, .. } = &first else {
            panic!("expected ambiguous");
        };
        assert_eq!(
            db.get_transfer_attempt(spend_id)
                .unwrap()
                .outcome
                .as_deref(),
            Some("ambiguous")
        );
        // Reconciliation retry passes the SAME id: no new attempt row.
        let seen = spend_id.clone();
        let second =
            execute_guarded_transfer(&db, "payout", 3, 42, Some(spend_id), |sid| async move {
                Ok(serde_json::json!({"transfer_id": sid.len()}))
            })
            .await
            .unwrap();
        let GuardedTransferOutcome::Succeeded {
            spend_id: reused, ..
        } = second
        else {
            panic!("expected success on reuse");
        };
        assert_eq!(reused, seen);
        assert_eq!(
            db.latest_transfer_attempt("payout", 3).unwrap().attempt_no,
            1
        );
    }

    #[tokio::test]
    async fn guarded_transfer_definite_retry_is_fresh() {
        let (db, _dir) = test_db("money_guarded_definite");
        let first = execute_guarded_transfer(&db, "refund", 9, 42, None, |_| async {
            Err("TRANSFER_FAILED".to_string())
        })
        .await
        .unwrap();
        let GuardedTransferOutcome::DefiniteRejected {
            spend_id: first_id, ..
        } = first
        else {
            panic!("expected definite rejection");
        };
        let second = execute_guarded_transfer(&db, "refund", 9, 42, None, |_| async {
            Ok(serde_json::json!({"transfer_id": 2}))
        })
        .await
        .unwrap();
        let GuardedTransferOutcome::Succeeded {
            spend_id: second_id,
            ..
        } = second
        else {
            panic!("expected success on retry");
        };
        assert_ne!(first_id, second_id);
        assert_eq!(
            db.latest_transfer_attempt("refund", 9).unwrap().attempt_no,
            2
        );
    }

    #[test]
    fn guarded_transfer_rejects_unknown_or_foreign_spend_id() {
        let (db, _dir) = test_db("money_guarded_foreign");
        // An unknown id has no persisted attempt: fail closed, no call.
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(execute_guarded_transfer(
            &db,
            "refund",
            1,
            42,
            Some("nope"),
            |_| async { Ok(serde_json::json!({})) },
        ));
        assert!(err.is_err());
    }

    #[test]
    fn refund_cap_and_idempotency_propagate() {
        use crate::db::{RefundCreateError, RefundInput};
        let (db, _dir) = test_db("money_cap_idem");
        db.save_invoice(
            "inv1",
            42,
            "5",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(99),
            None,
        );
        let input = RefundInput {
            seller_id: 42,
            invoice_id: Some("inv1"),
            user_id: Some(99),
            amount: "2",
            asset: "USDT",
            reason: None,
            idem_key: "api:one",
            created_by: 7,
            created_role: "owner",
        };
        let first = db.create_refund_guarded(input).unwrap();
        assert!(first.created_new);
        let replay = db
            .create_refund_guarded(RefundInput {
                idem_key: "api:one",
                ..input
            })
            .unwrap();
        assert_eq!(replay.id, first.id);
        assert!(!replay.created_new);
        let in_flight = db.create_refund_guarded(RefundInput {
            idem_key: "api:two",
            ..input
        });
        assert_eq!(in_flight.unwrap_err(), RefundCreateError::InFlight);
        // A distinct key whose request would breach the cumulative cap (after
        // the in-flight rows are voided) is refused with the spec code.
        db.update_refund_status(first.id, "void");
        let capped = db.create_refund_guarded(RefundInput {
            idem_key: "api:three",
            amount: "6",
            ..input
        });
        assert_eq!(capped.unwrap_err(), RefundCreateError::CapExceeded);
    }

    #[test]
    fn paid_ungranted_liability_uses_injected_escalation_cutoff() {
        use crate::db::RefundInput;
        let (db, _dir) = test_db("money_ungranted_liability");
        db.save_invoice(
            "inv72",
            42,
            "5",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(99),
            None,
        );
        assert!(db.mark_invoice_paid_ungranted("inv72"));
        // Cutoff is "ungranted at or before this instant": a future cutoff
        // includes the freshly stamped row (the sweeper passes now-72h).
        let past = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        // Unresolved: no auto-refund exists yet.
        assert_eq!(
            db.unresolved_ungranted_invoices(42, &past),
            vec!["inv72".to_string()]
        );
        // Queue the system auto-refund and execute it: the liability clears.
        let created = db
            .create_refund_guarded(RefundInput {
                seller_id: 42,
                invoice_id: Some("inv72"),
                user_id: None,
                amount: "5",
                asset: "USDT",
                reason: None,
                idem_key: "auto:ungranted:inv72",
                created_by: -1,
                created_role: "system",
            })
            .unwrap();
        assert!(created.created_new);
        assert_eq!(
            db.unresolved_ungranted_invoices(42, &past),
            vec!["inv72".to_string()]
        );
        db.update_refund_status(created.id, "executed");
        assert!(db.unresolved_ungranted_invoices(42, &past).is_empty());
    }

    #[test]
    fn full_vs_partial_refund_settlement_decision() {
        use crate::db::RefundInput;
        let (db, _dir) = test_db("money_full_partial");
        db.save_invoice(
            "invp",
            42,
            "5",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(99),
            None,
        );
        let full = db
            .create_refund_guarded(RefundInput {
                seller_id: 42,
                invoice_id: Some("invp"),
                user_id: None,
                amount: "5",
                asset: "USDT",
                reason: None,
                idem_key: "api:full",
                created_by: 7,
                created_role: "owner",
            })
            .unwrap();
        assert!(is_full_refund(&db, "invp"));
        db.update_refund_status(full.id, "void");
        let partial = db
            .create_refund_guarded(RefundInput {
                seller_id: 42,
                invoice_id: Some("invp"),
                user_id: None,
                amount: "3",
                asset: "USDT",
                reason: None,
                idem_key: "api:partial",
                created_by: 7,
                created_role: "owner",
            })
            .unwrap();
        let _ = partial;
        assert!(!is_full_refund(&db, "invp"));
    }

    #[test]
    fn refund_creator_reads_persisted_column_not_audit_window() {
        use crate::db::RefundInput;
        let (db, _dir) = test_db("money_creator_persisted");
        db.save_invoice(
            "invc",
            42,
            "5",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(99),
            None,
        );
        let made = db
            .create_refund_guarded(RefundInput {
                seller_id: 42,
                invoice_id: Some("invc"),
                user_id: None,
                amount: "5",
                asset: "USDT",
                reason: None,
                idem_key: "api:creator-persisted",
                created_by: 7,
                created_role: "owner",
            })
            .unwrap();
        // A conflicting (later) audit row must NOT win over the persisted
        // column — the whole point of F3 is that the creator cannot be aged
        // out or shadowed by audit history.
        db.log_action(999, "owner", "refund_create", "refund", made.id, None);
        assert_eq!(refund_creator(&db, made.id), Some((7, "owner".into())));
        assert!(creator_owns_refund(&db, 7, made.id));
        assert!(!creator_owns_refund(&db, 999, made.id));
        assert_eq!(refund_creator(&db, made.id + 12345), None);
        assert!(!creator_owns_refund(&db, 7, made.id + 12345));
    }

    #[test]
    fn refund_creator_falls_back_to_audit_only_for_legacy_sentinel() {
        let (db, _dir) = test_db("money_creator_sentinel");
        // Legacy `create_refund` writes the -1 / 'unknown' sentinel.
        let legacy_id = db.create_refund(42, None, None, "5", "USDT", None);
        assert!(legacy_id > 0);
        assert_eq!(db.refund_creator(legacy_id), Some((-1, "unknown".into())));
        // No audit event yet -> fail closed, no guessed creator.
        assert_eq!(refund_creator(&db, legacy_id), None);
        // The backfill-style audit event supplies the human actor.
        db.log_action(55, "seller", "refund_create", "refund", legacy_id, None);
        assert_eq!(refund_creator(&db, legacy_id), Some((55, "seller".into())));
        assert!(creator_owns_refund(&db, 55, legacy_id));
        assert!(!creator_owns_refund(&db, 7, legacy_id));
    }

    #[test]
    fn digest_hash_is_stable_and_order_independent() {
        let row = |row_type: &str, id: i64| crate::db::ReconciliationRow {
            row_type: row_type.to_string(),
            id,
            seller_id: 1,
            amount: "1".into(),
            asset: "USDT".into(),
            status: "executing".into(),
            since: "2026-11-20T08:00:00+00:00".into(),
            spend_id: None,
            overdue: false,
        };
        let a = digest_row_ids_hash(&[row("refund", 2), row("payout", 1)]);
        let b = digest_row_ids_hash(&[row("payout", 1), row("refund", 2)]);
        assert_eq!(a, b);
        assert_ne!(a, digest_row_ids_hash(&[row("refund", 2)]));
    }
}
