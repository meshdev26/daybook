#![allow(clippy::type_complexity)]
use crate::secrets::{MasterKey, SecretError, decrypt_secret, encrypt_secret};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Stored post row. Field positions (index access is load-bearing in
/// api.rs/web.rs/mcp::tools): 0 id, 1 seller_id, 2 chat_id, 3 message_id,
/// 4 kind, 5 content, 6 created_at, **7 sender** — Phase 2 appended `sender`
/// at the END (`'bot'` | `'user'`, user-identity design §1 sender audit);
/// every pre-existing index stayed valid.
type PostRow = (
    i64,
    i64,
    String,
    i32,
    String,
    Option<String>,
    String,
    String,
);
type ScheduledPostRow = (
    i64,
    i64,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    Option<i64>,
);
type InvoiceRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    i64,
);
// Row aliases used across pub fn signatures; pub so other modules can name
// them (the /buy resolver in handlers::commands returns these verbatim).
pub type PlanRow = (String, String, String, i64, Option<String>, bool, i64);
pub type PlanGlobalRow = (i64, i64, String, String, String, i64, Option<String>);

/// A single numeric money total cannot be formed when invoices use more
/// than one asset. Money amounts stay denominated per asset; callers must
/// render [`Db::revenue_by_asset`](Db::revenue_by_asset) lines instead of a
/// summed float.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixedAssets;

impl std::fmt::Display for MixedAssets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("mixed assets: no cross-asset money total is available")
    }
}

impl std::error::Error for MixedAssets {}

/// Booking row for the console table and `/mybookings` (web/server), adjusted to
/// the real columns of schema.sql: (bookings.id, bookings.booking_id public
/// token, buyer user_id, service name, slot starts_at, slot ends_at, status,
/// expires_at, service seller_id). starts_at/ends_at are None for legacy rows
/// without a slot; expires_at is None on legacy rows predating the hold
/// window; seller_id is None only if the service row itself is gone.
pub type BookingListRow = (
    i64,            // bookings.id
    String,         // bookings.booking_id (public 8-char token)
    i64,            // bookings.user_id (buyer)
    String,         // services.name
    Option<String>, // slots.starts_at
    Option<String>, // slots.ends_at
    String,         // bookings.status
    Option<String>, // bookings.expires_at
    Option<i64>,    // services.seller_id
);

/// How long an invoice may sit in 'granting' before pending_invoices()
/// treats the claim as wedged (claimer crashed between
/// claim_invoice_for_grant and finalize_invoice_paid) and re-queues it for
/// re-verification. Generous enough that a live granter — verify + grant +
/// finalize takes seconds — is never raced, and the claim stamps updated_at
/// so the clock starts at claim acquisition.
const WEDGED_GRANT_STALE_MINUTES: i64 = 5;
/// D3 (ARCHITECTURE-V1.4 §2): the booking hold IS the seller-response window —
/// `expires_at = min(now + BOOKING_HOLD_MINUTES, slot start)`. Single tuning
/// point for the whole booking state machine; pub so the bot copy (server) and
/// console copy (web) quote the same number instead of a hardcoded string.
pub const BOOKING_HOLD_MINUTES: i64 = 60;
/// TTL in minutes for one-time console setup links (D7): the DM'd link dies
/// unclaimed after this long.
pub const SETUP_LINK_TTL_MINUTES: i64 = 15;
/// Purpose tag one-time setup links carry in `web_pending_mfa.purpose` (D7);
/// pub because the claim routes (web) validate the same string on GET/POST.
pub const SETUP_LINK_PURPOSE: &str = "setup_link";

// ---- Money Safety Spec (approved batch, decisions 1/3/4/5) ----
// Internal operational timers, NOT customer SLAs (spec §3/§5, G7). Kept in
// the data layer as constants so the server's injectable-clock sweeper and verification's
// boundary tests quote the same numbers.
/// §5.1: executing rows older than this move to `needs_reconciliation`.
pub const RECONCILIATION_DUE_MINUTES: i64 = 30;
/// §5.2: needs-reconciliation rows older than this raise the blocking alert.
pub const RECONCILIATION_OVERDUE_HOURS: i64 = 24;
/// §3.3: retry cadence for `paid_ungranted` grant claims.
pub const PAID_UNGRANTED_RETRY_SECONDS: i64 = 30;
/// §3.4: paid-but-ungranted rows older than this queue an auto-refund.
pub const PAID_UNGRANTED_AUTO_REFUND_HOURS: i64 = 24;
/// §3.5: paid-but-ungranted rows older than this block new seller payouts.
pub const PAID_UNGRANTED_ESCALATION_HOURS: i64 = 72;
/// §1.2: all money amounts are base-10 decimals in integer minor units at
/// this scale (10^8). No f64, no `CAST(... AS REAL)` (CRYPTO_PAY.md:57-59).
const MONEY_MINOR_SCALE: u32 = 8;

/// Creation input for the guarded refund path (§1.2). Every new refund is
/// seller-scoped and carries an actor and an idempotency key; the server passes the
/// real actor, the legacy `create_refund` wrapper passes sentinels.
#[derive(Debug, Clone, Copy)]
pub struct RefundInput<'a> {
    pub seller_id: i64,
    pub invoice_id: Option<&'a str>,
    pub user_id: Option<i64>,
    pub amount: &'a str,
    pub asset: &'a str,
    pub reason: Option<&'a str>,
    pub idem_key: &'a str,
    pub created_by: i64,
    pub created_role: &'a str,
}

/// Outcome of a guarded refund creation. `Existing` is an idempotent replay
/// of the same `(invoice_id, idem_key)` and MUST NOT emit a second money
/// audit event (§1.2 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefundCreation {
    pub id: i64,
    pub created_new: bool,
}

/// Fail-closed creation errors. The HTTP/UX mapping is the server's; the variants
/// carry the spec's stable error codes in their doc comments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefundCreateError {
    /// 404 — no such invoice.
    InvoiceNotFound,
    /// 400 — invoice is not `paid`/`paid_ungranted`.
    InvoiceNotPaid,
    /// 400 — refund asset differs from the invoiced asset (§1.1 trigger).
    AssetMismatch,
    /// 409 `unquantifiable_legacy_refund` — a stored amount is negative,
    /// non-finite or not a base-10 decimal at scale 8.
    UnquantizableLegacy,
    /// 409 `refund_cap_exceeded` — cumulative counted refunds + request exceed
    /// the invoiced amount.
    CapExceeded,
    /// 409 `refund_in_flight` — a distinct key tried while another refund is
    /// pending/approved/executing/needs_reconciliation for the same invoice.
    InFlight,
    /// Any rusqlite failure (including a uniqueness-race loser).
    Database(String),
}

/// The three owner reconciliation outcomes (§5.3). The resulting row status
/// is the persisted outcome token: executed / approved / void. `resolution`
/// stores the human reason; `resolved_by`/`resolved_at` the auditor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationOutcome {
    Executed,
    RevertedToApproved,
    Void,
}

impl ReconciliationOutcome {
    fn status_for(self, row_type: &str) -> &'static str {
        match self {
            Self::Executed if row_type == "refund" => "executed",
            Self::Executed => "paid",
            Self::RevertedToApproved => "approved",
            Self::Void => "void",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileError {
    NotFound,
    /// Row is not `needs_reconciliation` (only that state may be resolved).
    NotDue,
    /// Append-only: a resolution was already recorded.
    AlreadyResolved,
    /// Owner must supply a non-empty reason (audited by server).
    ReasonRequired,
    Database(String),
}

/// One row in the §5.4 daily digest / §5.1-5.2 sweeps. `spend_id` is the
/// latest persisted attempt for the row (None only for a pre-spend-id legacy
/// row that never had an attempt recorded).
#[derive(Debug, Clone)]
pub struct ReconciliationRow {
    pub row_type: String,
    pub id: i64,
    pub seller_id: i64,
    pub amount: String,
    pub asset: String,
    pub status: String,
    pub since: String,
    pub spend_id: Option<String>,
    pub overdue: bool,
}

/// One append-only transfer attempt (§4.1).
#[derive(Debug, Clone)]
pub struct TransferAttemptRow {
    pub id: i64,
    pub row_type: String,
    pub row_id: i64,
    pub attempt_no: i64,
    pub spend_id: String,
    pub created_at: String,
    pub outcome: Option<String>,
    pub resolved_at: Option<String>,
    pub seller_id: i64,
}

// Legacy `create_refund` callers (api.rs / web finance) predate the idem-key
// contract; the wrapper synthesizes a unique sentinel key so the DB layer
// can still enforce in-flight uniqueness for them. The server replaces those callers
// with `create_refund_guarded` carrying the real actor/key.
static REFUND_IDEM_SEQ: AtomicU64 = AtomicU64::new(0);

// Named-field row (not a tuple alias) — a new public surface consumed by the
// web channels manager; seller_id 0 = global namespace, is_default flags the
// single global default.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRow {
    pub id: i64,
    pub channel: String,
    pub title: Option<String>,
    pub seller_id: i64,
    pub is_default: bool,
    pub created_at: String,
}
pub type ProductRow = (
    i64,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    i64,
);
type CodeRow = (i64, i64, Option<i64>, Option<i64>, i64, i64, Option<String>);
type CodeListRow = (String, Option<i64>, Option<i64>, i64, i64, i64, bool);
type SellerAiSettings = (Option<String>, Option<String>, Option<i64>, Option<i64>);
type CodeRedemptionRow = (i64, i64, String);
type AiTemplateRow = (
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);
type RefundRow = (
    i64,
    i64,
    Option<String>,
    Option<i64>,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
);
type PayoutRow = (
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
);
type AuditLogRow = (
    i64,
    i64,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);
type ApiKeyRow = (
    i64,
    i64,
    Option<String>,
    String,
    Option<String>,
    i64,
    Option<String>,
);
type ApiKeyListRow = (i64, i64, Option<String>, String, i64, Option<String>);
type McpApprovalRow = (
    i64,            // id
    String,         // tool
    String,         // args_json
    String,         // status
    String,         // created_at
    String,         // expires_at
    Option<String>, // decided_at
    Option<String>, // executed_at
);
type McpAuditRow = (i64, String, String, bool, Option<String>, String);

// ---- Cross-cutting moderation (multibot foundation, Step 4 storage) ----

/// Longest accepted policy term / opaque reference. Over-long values are
/// refused at write time (the console also validates) so stored rows stay
/// minimal and the audit trail cannot be used as a message dump.
pub const MODERATION_MAX_TERM_LEN: usize = 200;
/// Longest stored appeal justification / decision reason code. This is NEVER
/// the filtered message body — it is a short reason string or machine code.
pub const MODERATION_MAX_REASON_LEN: usize = 280;

/// FALLBACK DEFAULTS for the owner content-policy floor — NOT the shipped
/// floor. The live floor is the persistent `moderation_global_floor` table,
/// which the owner writes through the (owner-gated) console. These constants
/// are used ONLY when that table cannot be read (e.g. an older schema or a
/// runtime read failure): the loosest global clamp and an empty baseline term
/// set, so the fallback leaves existing seller rules unchanged. Once the table
/// is available the persistent terms replace this empty baseline entirely.
///
/// The floor is NON-OVERRIDABLE: seller and per-bot rules may only raise
/// severity, and a floor term can never be removed or weakened by the
/// allow-list. `MODERATION_FLOOR_MODE` is the global minimum clamp applied to
/// every term; each persistent floor term additionally carries its own mode
/// (clamped to at least this value).
pub const MODERATION_FLOOR_MODE: &str = "warn";
pub const MODERATION_FLOOR_TERMS: &[&str] = &[];

/// Enforcement severity, loosest (1) to strictest (3). This is the single
/// ordering used by precedence resolution. An unknown stored value maps to
/// the loosest tier so a bad value can never silently escalate a decision.
pub fn moderation_mode_severity(mode: &str) -> u8 {
    match mode {
        "hide" => 3,
        "review" => 2,
        "warn" => 1,
        _ => 1,
    }
}
fn moderation_mode_from_severity(severity: u8) -> &'static str {
    match severity {
        3 => "hide",
        2 => "review",
        _ => "warn",
    }
}
fn is_moderation_mode(mode: &str) -> bool {
    matches!(mode, "hide" | "warn" | "review")
}
fn is_moderation_appeal_status(status: &str) -> bool {
    matches!(status, "pending" | "approved" | "upheld")
}
/// Trim + validate a policy term. SQL metacharacters are kept as literal DATA
/// (parameterized statements make them inert); NUL bytes, empty and
/// over-long terms are refused. This is "injection refusal" at the storage
/// boundary: the term can never break out of its bind parameter.
fn normalize_moderation_term(term: &str) -> Option<String> {
    let t = term.trim();
    if t.is_empty() || t.len() > MODERATION_MAX_TERM_LEN || t.contains('\0') {
        return None;
    }
    Some(t.to_owned())
}
fn normalize_moderation_ref(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() || v.len() > MODERATION_MAX_TERM_LEN || v.contains('\0') {
        return None;
    }
    Some(v.to_owned())
}
fn trim_moderation_reason(reason: Option<&str>) -> Option<String> {
    reason.and_then(|r| {
        let cleaned: String = r
            .chars()
            .filter(|c| *c != '\0')
            .take(MODERATION_MAX_REASON_LEN)
            .collect();
        let cleaned = cleaned.trim().to_owned();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    })
}

/// One seller/override rule feeding precedence resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationTermInput {
    pub term: String,
    pub mode: String,
}
/// One effective term after floor + seller + bot precedence + allow-list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveModerationTerm {
    pub term: String,
    pub mode: String,
}
/// The resolved policy a caller (server hook) evaluates a message against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveModerationPolicy {
    pub seller_id: i64,
    pub bot_id: Option<i64>,
    /// Non-overridable minimum enforcement mode (owner floor).
    pub floor_mode: String,
    /// Effective blocked terms, each with its resolved mode; sorted by term
    /// for deterministic matching and diffing.
    pub terms: Vec<EffectiveModerationTerm>,
    /// True when the bot uses "Custom rules for this bot"; false when it is
    /// inheriting the seller's rules.
    pub bot_override_active: bool,
    /// Fail-closed signal. True when moderation storage was not ready at
    /// startup (a DDL failure) or the persistent floor could not be read, so
    /// this policy is incomplete. server hooks MUST treat true as "hold the
    /// message for review" rather than evaluating against a partial/empty
    /// policy — an empty policy here means enforcement is unavailable, not
    /// that the message is safe.
    pub enforcement_degraded: bool,
}

/// One owner-enforced global floor term. Unlike the seller-scoped structs,
/// this has NO `seller_id`: the floor applies to every seller and bot and is
/// writable only through explicit owner-gated `Db` methods below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationGlobalFloorTerm {
    pub id: i64,
    pub term: String,
    pub mode: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationBlockedWord {
    pub id: i64,
    pub seller_id: i64,
    pub term: String,
    pub mode: String,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationAllowWord {
    pub id: i64,
    pub seller_id: i64,
    pub term: String,
    pub created_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationBotOverrideTerm {
    pub id: i64,
    pub seller_id: i64,
    pub bot_id: i64,
    pub term: String,
    pub mode: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationAppeal {
    pub id: i64,
    pub seller_id: i64,
    pub bot_id: Option<i64>,
    pub decision_id: Option<i64>,
    pub message_ref: String,
    pub reason: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModerationDecision {
    pub id: i64,
    pub seller_id: i64,
    pub bot_id: Option<i64>,
    pub term: Option<String>,
    pub action: String,
    pub reason: Option<String>,
    pub message_ref: Option<String>,
    pub created_at: String,
}

/// Pure precedence resolver. The floor is passed in by the caller so the
/// ladder can be unit-tested against a non-empty floor without changing
/// shipped data. Production passes the persistent `moderation_global_floor`
/// rows as `floor_terms` (falling back to the compile-time defaults when the
/// table is unavailable).
///
/// Rules, in order:
/// 1. Each floor term starts at its own owner-set mode, clamped to at least
///    `floor_mode` (the global minimum), and is non-removable.
/// 2. Seller terms are added; a repeated term takes the STRICTER mode
///    (`max`), never a looser one, and nothing drops below the floor.
/// 3. Per-bot terms follow the same stricter-only rule (the caller passes an
///    empty slice when the bot inherits).
/// 4. Allow-list terms are removed from the effective set — EXCEPT floor
///    terms, which "stay above the floor" by construction. A seller rule that
///    happens to equal a floor term can raise it further but never weaken it.
pub fn apply_moderation_precedence(
    floor_mode: &str,
    floor_terms: &[ModerationTermInput],
    seller_terms: &[ModerationTermInput],
    bot_terms: &[ModerationTermInput],
    allow_terms: &[String],
) -> Vec<EffectiveModerationTerm> {
    use std::collections::{BTreeMap, BTreeSet};

    let floor_sev = moderation_mode_severity(floor_mode);
    let mut effective: BTreeMap<String, u8> = BTreeMap::new();
    let floor_set: BTreeSet<String> = floor_terms.iter().map(|t| t.term.clone()).collect();
    for rule in floor_terms {
        let sev = moderation_mode_severity(&rule.mode).max(floor_sev);
        effective.insert(rule.term.clone(), sev);
    }
    let mut raise = |terms: &[ModerationTermInput]| {
        for rule in terms {
            let sev = moderation_mode_severity(&rule.mode).max(floor_sev);
            effective
                .entry(rule.term.clone())
                .and_modify(|current| *current = (*current).max(sev))
                .or_insert(sev);
        }
    };
    raise(seller_terms);
    raise(bot_terms);
    for allowed in allow_terms {
        if !floor_set.contains(allowed) {
            effective.remove(allowed);
        }
    }
    effective
        .into_iter()
        .map(|(term, sev)| EffectiveModerationTerm {
            term,
            mode: moderation_mode_from_severity(sev).to_owned(),
        })
        .collect()
}

/// SHA-256 hex digest of `input` — the one-time-credential storage recipe:
/// api_keys.key_hash, web_sessions.token_hash, web_pending_mfa.token_hash and
/// mcp_tokens.token_hash all store the hex digest, never the raw secret.
fn sha256_hex(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    let bytes = h.finalize();
    let mut s = String::with_capacity(64);
    for b in bytes.iter() {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

// Invariant: every lock of `conn` recovers from poisoning via
// `.unwrap_or_else(|poisoned| poisoned.into_inner())` — a panic in one caller
// must never turn into a permanent lock failure for every other caller
// (background jobs, bot, web). Never `.unwrap()` this mutex.
#[derive(Debug, Clone)]
pub struct BotRecord {
    pub id: i64,
    pub key: String,
    pub username: Option<String>,
    pub owner_user_id: i64,
    pub default_channel_id: Option<i64>,
    pub enabled: bool,
    pub created_at: String,
    /// Console display name; '' means "show the key instead".
    pub name: String,
    /// Product/capability type (multibot foundation). Defaults to
    /// `shop_bot` so every legacy row keeps its existing behavior.
    pub bot_type: String,
}

/// Single mapping for every `bots` SELECT, so a new column only has to be
/// added to the column lists (and here) once. Column order is shared by
/// `get_bot`, `get_bot_by_key`, `list_bots`, and the typed listing methods.
fn bot_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<BotRecord> {
    Ok(BotRecord {
        id: r.get(0)?,
        key: r.get(1)?,
        username: r.get(2)?,
        owner_user_id: r.get(3)?,
        default_channel_id: r.get(4)?,
        enabled: r.get::<_, i64>(5)? != 0,
        created_at: r.get(6)?,
        name: r.get(7)?,
        bot_type: r.get(8)?,
    })
}

pub struct Db {
    conn: Mutex<Connection>,
    /// Starts false (fail closed) and is flipped true only after every
    /// moderation DDL statement in `migrate()` succeeds. A DDL failure leaves
    /// it false so `resolve_effective_policy` can surface `enforcement_degraded`
    /// instead of silently returning an empty, permissive policy.
    moderation_storage_ready: AtomicBool,
}

#[allow(dead_code)]
impl Db {
    pub fn new(path: &str) -> Result<Self, rusqlite::Error> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        // Production pragmas: WAL for concurrent readers, busy timeout, FK enforcement.
        // Best-effort: in-memory dbs (tests) cannot use WAL.
        let _ = conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;",
        );
        let db = Self {
            conn: Mutex::new(conn),
            moderation_storage_ready: AtomicBool::new(false),
        };
        db.init_schema()?;
        // Migration remains best-effort overall so legacy databases still open
        // (see `migrate`). Moderation DDL failures are logged and recorded in
        // `moderation_storage_ready`; they never panic here.
        let _ = db.migrate();
        // Money-safety DDL is deliberately NOT best-effort. A legacy DB that
        // already holds two in-flight refunds for one invoice fails closed
        // here with the offending invoice ids (spec §1.4) until an owner voids
        // the duplicates through the audited resolution path. Clean legacy
        // and fresh DBs are unchanged: every statement is additive and
        // idempotent.
        db.migrate_money_safety()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), rusqlite::Error> {
        let schema = include_str!("../schema.sql");
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute_batch(schema)?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // ensure seller columns exist in older dbs (best-effort)
        let _ = conn.execute(
            "ALTER TABLE plans ADD COLUMN seller_id INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE subscriptions ADD COLUMN seller_id INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE activation_codes ADD COLUMN seller_id INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE activation_codes ADD COLUMN revoked INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE invoices ADD COLUMN seller_id INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE posts ADD COLUMN seller_id INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS sellers (user_id INTEGER PRIMARY KEY, username TEXT, default_channel TEXT, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("ALTER TABLE sellers ADD COLUMN ai_provider TEXT", []);
        let _ = conn.execute("ALTER TABLE sellers ADD COLUMN ai_model TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE sellers ADD COLUMN daily_token_budget INTEGER",
            [],
        );
        let _ = conn.execute("ALTER TABLE sellers ADD COLUMN rpm_limit INTEGER", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS web_accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, salt TEXT NOT NULL, role TEXT NOT NULL, seller_id INTEGER, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS web_sessions (token_hash TEXT PRIMARY KEY, account_id INTEGER NOT NULL, role TEXT NOT NULL, seller_id INTEGER, expires_at TEXT NOT NULL, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS refunds (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, invoice_id TEXT, user_id INTEGER, amount TEXT NOT NULL, asset TEXT NOT NULL, reason TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, updated_at TEXT NOT NULL, processed_at TEXT, processed_by INTEGER)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS payouts (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, amount TEXT NOT NULL, asset TEXT NOT NULL, destination TEXT, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, updated_at TEXT NOT NULL, processed_at TEXT, processed_by INTEGER)", []);
        let _ = conn.execute("ALTER TABLE refunds ADD COLUMN processed_at TEXT", []);
        let _ = conn.execute("ALTER TABLE refunds ADD COLUMN processed_by INTEGER", []);
        let _ = conn.execute("ALTER TABLE payouts ADD COLUMN processed_at TEXT", []);
        let _ = conn.execute("ALTER TABLE payouts ADD COLUMN processed_by INTEGER", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, actor_id INTEGER NOT NULL, actor_role TEXT NOT NULL, action TEXT NOT NULL, target_type TEXT NOT NULL, target_id TEXT, metadata TEXT, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS api_keys (id INTEGER PRIMARY KEY AUTOINCREMENT, account_id INTEGER NOT NULL, name TEXT, key_hash TEXT NOT NULL UNIQUE, key_prefix TEXT, scopes TEXT, revoked INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, expires_at TEXT, last_used_at TEXT)", []);
        // Capability verticals. File shares store only a Telegram file_id
        // reference (the bytes stay in Telegram); automation rules are
        // keyword-triggered auto-replies. Both are seller/bot scoped and use
        // the shared audit_log for every state change and firing.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS file_shares (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, bot_id INTEGER NOT NULL DEFAULT 0, file_id TEXT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL DEFAULT '', token TEXT NOT NULL UNIQUE, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS file_share_grants (share_id INTEGER NOT NULL, user_id INTEGER NOT NULL, created_at TEXT NOT NULL, PRIMARY KEY (share_id, user_id))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_file_shares_scope ON file_shares(seller_id, bot_id)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS automation_rules (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, bot_id INTEGER NOT NULL DEFAULT 0, keyword TEXT NOT NULL, response TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_automation_rules_scope ON automation_rules(seller_id, bot_id)", []);
        // Production indexes for multi-tenant scoping and lookups.
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_plans_seller ON plans(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_subs_seller ON subscriptions(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_subs_user ON subscriptions(user_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_invoices_seller ON invoices(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_codes_seller ON activation_codes(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_posts_seller ON posts(seller_id)",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_posts_id ON posts(id)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_web_sessions_expires ON web_sessions(expires_at)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_invoices_status ON invoices(status)",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_subscriptions_status_expires ON subscriptions(status, expires_at)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_refunds_seller ON refunds(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_payouts_seller ON payouts(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_log(created_at)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_api_keys_account ON api_keys(account_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS scheduled_posts (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, chat_id TEXT NOT NULL, content TEXT, kind TEXT NOT NULL, scheduled_at TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, posted_message_id INTEGER)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_scheduled_posts_status_time ON scheduled_posts(status, scheduled_at)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scheduled_posts_seller ON scheduled_posts(seller_id)",
            [],
        );
        // v1.2 products: digital/physical goods sold through the invoice
        // pipeline; grants are lifetime (no expiry column) and one per invoice.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS products (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, name TEXT NOT NULL, price TEXT NOT NULL, asset TEXT NOT NULL, kind TEXT NOT NULL CHECK (kind IN ('digital_link','digital_file','physical')), delivery TEXT, stock INTEGER, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, UNIQUE(seller_id, name))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS product_grants (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, user_id INTEGER NOT NULL, product_id INTEGER NOT NULL, invoice_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL DEFAULT 'active', created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_products_seller ON products(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_product_grants_seller ON product_grants(seller_id)",
            [],
        );
        // v1.3 orders: physical fulfillment tracking. One order per purchase
        // (UNIQUE invoice_id); additive-only like every migration here.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS orders (id INTEGER PRIMARY KEY AUTOINCREMENT, order_id TEXT NOT NULL UNIQUE, seller_id INTEGER NOT NULL, product_id INTEGER NOT NULL, buyer_user_id INTEGER NOT NULL, invoice_id TEXT NOT NULL UNIQUE, status TEXT NOT NULL DEFAULT 'pending', tracking TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_orders_seller_status ON orders(seller_id, status)",
            [],
        );
        // v1.3 MCP: server-side approval gate for sensitive tools, plus a
        // dedicated audit trail for every tool call outcome.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS mcp_approvals (id INTEGER PRIMARY KEY AUTOINCREMENT, tool TEXT NOT NULL, args_json TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', created_at TEXT NOT NULL, expires_at TEXT NOT NULL, decided_at TEXT, executed_at TEXT)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_approvals_status ON mcp_approvals(status)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS mcp_audit (id INTEGER PRIMARY KEY AUTOINCREMENT, tool TEXT NOT NULL, role TEXT NOT NULL, ok INTEGER NOT NULL, detail TEXT, created_at TEXT NOT NULL, seller_id INTEGER)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_audit_created ON mcp_audit(created_at)",
            [],
        );
        // v2 per-seller MCP tokens (not yet wired into auth). Additive.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS mcp_tokens (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, token_hash TEXT NOT NULL UNIQUE, token_prefix TEXT, name TEXT, revoked INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, expires_at TEXT, last_used_at TEXT)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_tokens_hash ON mcp_tokens(token_hash)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_mcp_tokens_seller ON mcp_tokens(seller_id)",
            [],
        );
        // v1.3.x channels: managed-channel store (schema.sql declares it for
        // fresh databases; this covers legacy ones). One-time seed: an
        // existing install's default channel lives in the legacy free-text
        // `channel` config key, and on the first run after this migration it
        // becomes the table's single default global row. Tracked with a
        // config marker so it runs exactly once per database — removing all
        // channels later must not resurrect the legacy value on restart. The
        // legacy key itself is kept (never deleted): `default_channel` falls
        // back to it honestly.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS channels (id INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL UNIQUE COLLATE NOCASE, seller_id INTEGER NOT NULL DEFAULT 0, is_default INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_channels_seller ON channels(seller_id)",
            [],
        );
        // Phase 1: promo codes + shopping cart (additive)
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS promo_codes (code TEXT NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, promo_type TEXT NOT NULL CHECK (promo_type IN ('percent','fixed')), value REAL NOT NULL, currency TEXT, max_uses INTEGER, uses INTEGER NOT NULL DEFAULT 0, min_purchase REAL NOT NULL DEFAULT 0, scope TEXT NOT NULL DEFAULT 'all', scope_id INTEGER, expires_at TEXT, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, PRIMARY KEY (code, seller_id))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS promo_redemptions (id INTEGER PRIMARY KEY AUTOINCREMENT, code TEXT NOT NULL, seller_id INTEGER NOT NULL, buyer_id INTEGER NOT NULL, invoice_id TEXT, discount_amount REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL)", []);
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_promo_redemptions_seller ON promo_redemptions(seller_id)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS cart_items (id INTEGER PRIMARY KEY AUTOINCREMENT, buyer_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, item_type TEXT NOT NULL CHECK (item_type IN ('plan','product')), item_id INTEGER NOT NULL, qty INTEGER NOT NULL DEFAULT 1, added_at TEXT NOT NULL, UNIQUE (buyer_id, seller_id, item_type, item_id))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_cart_items_buyer ON cart_items(buyer_id, seller_id)",
            [],
        );
        // Phase 2: referrals + bonus/loyalty balance (additive)
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS referrals (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL DEFAULT 0, referrer_id INTEGER NOT NULL, referee_id INTEGER NOT NULL, code TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending', reward_type TEXT, reward_value REAL NOT NULL DEFAULT 0, created_at TEXT NOT NULL, rewarded_at TEXT, UNIQUE (seller_id, referee_id))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_referrals_seller ON referrals(seller_id)",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_referrals_referrer ON referrals(seller_id, referrer_id)", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS referral_codes (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL DEFAULT 0, user_id INTEGER NOT NULL, code TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE (seller_id, user_id), UNIQUE (seller_id, code))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS bonus_balances (user_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, balance_minor INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL, PRIMARY KEY (user_id, seller_id))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS bonus_ledger (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, amount_minor INTEGER NOT NULL, reason TEXT, created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_bonus_ledger_user ON bonus_ledger(user_id, seller_id)",
            [],
        );
        // Multi-bot (v1.6): bots table + bot_id scoping (additive)
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS bots (id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT NOT NULL UNIQUE, username TEXT, owner_user_id INTEGER NOT NULL, default_channel_id INTEGER, enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, name TEXT NOT NULL DEFAULT '')", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_bots_owner ON bots(owner_user_id)",
            [],
        );
        // Bot display name (v1.7): console label; '' falls back to the key.
        // schema.sql declares the column for fresh databases; this covers
        // legacy ones. Additive + idempotent like bot_id above — the ALTER
        // errors out (ignored) once the column exists.
        let _ = conn.execute(
            "ALTER TABLE bots ADD COLUMN name TEXT NOT NULL DEFAULT ''",
            [],
        );
        // Multibot foundation (v2): product/capability type on each bot.
        // Additive + idempotent, exactly like `name` above: SQLite fills
        // existing rows with the NOT NULL DEFAULT on ALTER, so legacy bots
        // are backfilled to 'shop_bot' without a destructive rewrite. The
        // defensive UPDATE normalizes NULL/'' values a partially-migrated
        // writer could have left, and the index serves typed listing.
        let _ = conn.execute(
            "ALTER TABLE bots ADD COLUMN bot_type TEXT NOT NULL DEFAULT 'shop_bot'",
            [],
        );
        let _ = conn.execute(
            "UPDATE bots SET bot_type = 'shop_bot' WHERE bot_type IS NULL OR TRIM(bot_type) = ''",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_bots_type ON bots(bot_type)",
            [],
        );
        let _ = conn.execute("ALTER TABLE sellers ADD COLUMN bot_id INTEGER", []);
        let _ = conn.execute("ALTER TABLE channels ADD COLUMN bot_id INTEGER", []);
        let _ = conn.execute("ALTER TABLE admins ADD COLUMN bot_id INTEGER", []);
        let _ = conn.execute(
            "ALTER TABLE web_sessions ADD COLUMN selected_bot_id INTEGER",
            [],
        );
        let _ = conn.execute("ALTER TABLE mcp_audit ADD COLUMN bot_id INTEGER", []);
        // Personal-account identity (Phase 1): per-channel sender binding.
        // 'bot' (default) posts via the resolved BotContext; 'user' posts via
        // the operator's MTProto identity (channels.sender_identity, ratified
        // user-identity design §1). Additive + idempotent like bot_id above.
        let _ = conn.execute(
            "ALTER TABLE channels ADD COLUMN sender_identity TEXT NOT NULL DEFAULT 'bot'",
            [],
        );
        let _ = conn.execute("ALTER TABLE channels ADD COLUMN title TEXT", []);
        // Personal-account identity (Phase 2): posts sender audit. Every
        // stored post records who sent it — 'bot' (default; every Bot-API
        // path) or 'user' (the operator's MTProto identity via
        // publish_as_user). Additive + idempotent like the column above;
        // SQLite fills existing rows with the DEFAULT on ALTER.
        let _ = conn.execute(
            "ALTER TABLE posts ADD COLUMN sender TEXT NOT NULL DEFAULT 'bot'",
            [],
        );
        // Phase 3: CRM + support tickets (additive)
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS customers (user_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, first_seen TEXT NOT NULL, last_seen TEXT NOT NULL, tags TEXT, notes TEXT, total_spent REAL NOT NULL DEFAULT 0, order_count INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'active', PRIMARY KEY (user_id, seller_id))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_customers_seller ON customers(seller_id)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS tickets (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL DEFAULT 0, buyer_id INTEGER NOT NULL, subject TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'open', created_at TEXT NOT NULL, updated_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tickets_seller ON tickets(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_tickets_buyer ON tickets(buyer_id)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS ticket_messages (id INTEGER PRIMARY KEY AUTOINCREMENT, ticket_id INTEGER NOT NULL, sender_role TEXT NOT NULL, sender_id INTEGER NOT NULL, text TEXT NOT NULL, created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ticket_messages_ticket ON ticket_messages(ticket_id)",
            [],
        );
        // Phase 4: recovery & engagement (additive)
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS cart_abandonment (buyer_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, abandoned_at TEXT NOT NULL, reminded_count INTEGER NOT NULL DEFAULT 0, reminded_at TEXT, recovered_at TEXT, PRIMARY KEY (buyer_id, seller_id))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS reorder_schedules (id INTEGER PRIMARY KEY AUTOINCREMENT, buyer_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, product_id INTEGER NOT NULL, interval_days INTEGER NOT NULL, last_ordered_at TEXT, next_reminder_at TEXT, enabled INTEGER NOT NULL DEFAULT 1, UNIQUE (buyer_id, seller_id, product_id))", []);
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS reviews (id INTEGER PRIMARY KEY AUTOINCREMENT, buyer_id INTEGER NOT NULL, seller_id INTEGER NOT NULL DEFAULT 0, product_id INTEGER NOT NULL, rating INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5), text TEXT, approved INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL, UNIQUE (buyer_id, product_id))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_reviews_product ON reviews(seller_id, product_id)",
            [],
        );
        let channels_seeded = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'migration_channels_seed_v1'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        if channels_seeded.is_none() {
            let legacy: Option<String> = conn
                .query_row("SELECT value FROM config WHERE key = 'channel'", [], |r| {
                    r.get(0)
                })
                .optional()
                .ok()
                .flatten();
            let channel_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))
                .unwrap_or(1); // probe failure: do not seed on top of an unknown state
            if channel_count == 0
                && let Some(ch) = legacy.filter(|s| !s.trim().is_empty())
            {
                let ins = conn.execute(
                    "INSERT OR IGNORE INTO channels(channel,seller_id,is_default,created_at) VALUES(?1,0,1,?2)",
                    params![ch.trim(), Utc::now().to_rfc3339()],
                );
                if ins.is_ok() {
                    tracing::info!(
                        "channels: seeded default channel '{ch}' from the legacy config key"
                    );
                }
            }
            let _ = conn.execute(
                "INSERT OR REPLACE INTO config(key,value,updated_at) VALUES('migration_channels_seed_v1','1',?1)",
                params![Utc::now().to_rfc3339()],
            );
        }
        // invoices.product_id: nullable item reference selecting the grant
        // branch (NULL = existing plan path). Idempotent column add.
        let invoices_has_product_id = conn
            .query_row(
                "SELECT name FROM pragma_table_info('invoices') WHERE name = 'product_id'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        if invoices_has_product_id.is_none() {
            let _ = conn.execute("ALTER TABLE invoices ADD COLUMN product_id INTEGER", []);
        }
        // v1.4 mcp_audit.seller_id (ARCHITECTURE-V1.4 §3, D6): which seller
        // principal a tool call ran as — owner rows stay NULL. Idempotent
        // additive column add, the invoices.product_id precedent; appended
        // last so legacy (ALTERed) and fresh (schema.sql) layouts match.
        let mcp_audit_has_seller_id = conn
            .query_row(
                "SELECT name FROM pragma_table_info('mcp_audit') WHERE name = 'seller_id'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        if mcp_audit_has_seller_id.is_none() {
            let _ = conn.execute("ALTER TABLE mcp_audit ADD COLUMN seller_id INTEGER", []);
        }
        // Retry-cap counters (2026-10-25 review, "leaving for retry" forever):
        // the invoice verifier and the scheduled-post sender need a bounded
        // number of attempts with a terminal state. A COLUMN (not an in-memory
        // map keyed by invoice id) so the cap survives restarts — an
        // in-memory counter would give a poison invoice a fresh budget every
        // restart. Additive + idempotent in the house pattern; DEFAULT 0
        // back-fills existing rows, so no INSERT site changes.
        let _ = conn.execute(
            "ALTER TABLE invoices ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE scheduled_posts ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0",
            [],
        );
        // One subscription per invoice: guards against double-granting. If
        // historical duplicate grants exist the index cannot be created --
        // warn and continue (never fail startup over it).
        if let Err(e) = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_subscriptions_invoice ON subscriptions(invoice_id)",
            [],
        ) {
            tracing::warn!(
                "cannot enforce one-subscription-per-invoice (duplicate historical grants?): {}",
                e
            );
        }
        // Plan namespace uniqueness on (seller_id, name) — 2026-10-25 review
        // HIGH. Fresh databases declare UNIQUE(seller_id, name) in schema.sql;
        // legacy databases only got plans.seller_id via
        // ALTER ... DEFAULT 0 below, so same-name plans inside ONE seller's
        // namespace can exist there. seller_id is NOT NULL DEFAULT 0: 0 is the
        // admin/owner global namespace, NULL cannot be stored.
        //
        // Dedupe first, keeping the LOWEST id per (seller_id, name) like the
        // invoice unique-index remediation: subscriptions and activation
        // codes pointing at a dropped duplicate are re-pointed first so no
        // reference orphans, then the duplicates are deleted (warn with
        // counts). Only then create the unique index — skipped entirely when
        // the schema-level constraint already provides it (pragma_index_list
        // origin 'u'), so fresh databases don't carry a redundant index on
        // the plan-insert path. As with every migration here: a failure
        // warns and continues; startup never fails over it.
        let dup_groups: Vec<(i64, String, i64)> = match conn.prepare(
            "SELECT seller_id, name, COUNT(*) FROM plans GROUP BY seller_id, name HAVING COUNT(*) > 1",
        ) {
            Ok(mut stmt) => match stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            }) {
                Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                Err(_) => Vec::new(),
            },
            Err(e) => {
                tracing::warn!("plan namespace dedupe check skipped: {}", e);
                Vec::new()
            }
        };
        for (seller_id, name, count) in dup_groups {
            let keep: Option<i64> = conn
                .query_row(
                    "SELECT MIN(id) FROM plans WHERE seller_id=?1 AND name=?2",
                    params![seller_id, name],
                    |r| r.get(0),
                )
                .ok();
            let Some(keep) = keep else {
                continue;
            };
            // Re-point references before deleting so plan joins never orphan.
            let subs = conn.execute(
                "UPDATE subscriptions SET plan_id=?3 WHERE plan_id IN (SELECT id FROM plans WHERE seller_id=?1 AND name=?2 AND id != ?3)",
                params![seller_id, name, keep],
            );
            let codes = conn.execute(
                "UPDATE activation_codes SET plan_id=?3 WHERE plan_id IN (SELECT id FROM plans WHERE seller_id=?1 AND name=?2 AND id != ?3)",
                params![seller_id, name, keep],
            );
            let deleted = conn.execute(
                "DELETE FROM plans WHERE seller_id=?1 AND name=?2 AND id != ?3",
                params![seller_id, name, keep],
            );
            tracing::warn!(
                "plan namespace: seller {} plan '{}' had {} rows; kept lowest id {}, removed {}, re-pointed {} subscription(s) and {} activation code(s)",
                seller_id,
                name,
                count,
                keep,
                deleted.unwrap_or(0),
                subs.unwrap_or(0),
                codes.unwrap_or(0),
            );
        }
        let table_unique_constraint: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE \"unique\"=1 AND origin='u'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(1); // assume the constraint exists on probe failure: creating a
        // redundant index is harmless, dropping uniqueness is not
        if table_unique_constraint == 0
            && let Err(e) = conn.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_plans_seller_name ON plans(seller_id, name)",
                [],
            )
        {
            tracing::warn!(
                "cannot enforce per-seller plan name uniqueness (duplicate rows above?): {}",
                e
            );
        }
        // Bookings (v2): services / slots / bookings.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS services (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, name TEXT NOT NULL, price TEXT NOT NULL, asset TEXT NOT NULL, duration_minutes INTEGER NOT NULL DEFAULT 60, channel_id TEXT, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, UNIQUE(seller_id, name))", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_services_seller ON services(seller_id)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS slots (id INTEGER PRIMARY KEY AUTOINCREMENT, service_id INTEGER NOT NULL, seller_id INTEGER NOT NULL, starts_at TEXT NOT NULL, ends_at TEXT, capacity INTEGER NOT NULL DEFAULT 1, booked INTEGER NOT NULL DEFAULT 0, status TEXT NOT NULL DEFAULT 'available', held_until TEXT, created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_slots_service ON slots(service_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_slots_status ON slots(status)",
            [],
        );
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS bookings (id INTEGER PRIMARY KEY AUTOINCREMENT, booking_id TEXT NOT NULL UNIQUE, seller_id INTEGER NOT NULL, service_id INTEGER NOT NULL, slot_id INTEGER, user_id INTEGER NOT NULL, status TEXT NOT NULL DEFAULT 'held', invoice_id TEXT, expires_at TEXT, created_at TEXT NOT NULL)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_bookings_seller ON bookings(seller_id)",
            [],
        );
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_bookings_status ON bookings(status)",
            [],
        );
        // AI-assisted post drafts.
        let _ = conn.execute("CREATE TABLE IF NOT EXISTS ai_post_drafts (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, channel TEXT NOT NULL, template TEXT, content TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'draft', created_at TEXT NOT NULL, updated_at TEXT NOT NULL, published_message_id INTEGER)", []);
        let _ = conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_ai_post_drafts_seller ON ai_post_drafts(seller_id)",
            [],
        );

        // Phase 4 MFA: additive columns + tables.
        let _ = conn.execute("ALTER TABLE web_accounts ADD COLUMN totp_secret TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE web_accounts ADD COLUMN totp_enabled INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE web_accounts ADD COLUMN totp_last_used INTEGER",
            [],
        );
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS web_pending_mfa (token_hash TEXT PRIMARY KEY, account_id INTEGER NOT NULL, purpose TEXT NOT NULL, totp_secret TEXT, expires_at TEXT NOT NULL, created_at TEXT NOT NULL)",
            [],
        );
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS web_recovery_codes (id INTEGER PRIMARY KEY AUTOINCREMENT, account_id INTEGER NOT NULL, code_hash TEXT NOT NULL, used INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL)",
            [],
        );

        // One-time credential rotation (tracked via the config table so it
        let rotated = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'migration_rotate_tokens_v1'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        if rotated.is_none() {
            let _ = conn.execute_batch("BEGIN IMMEDIATE");
            let del_sessions = conn.execute("DELETE FROM web_sessions", []);
            let del_keys = conn.execute("DELETE FROM api_keys", []);
            let mark = conn.execute(
                "INSERT OR REPLACE INTO config(key,value,updated_at) VALUES('migration_rotate_tokens_v1','1',?1)",
                params![Utc::now().to_rfc3339()],
            );
            match (del_sessions, del_keys, mark) {
                (Ok(ns), Ok(nk), Ok(_)) => {
                    let _ = conn.execute_batch("COMMIT");
                    if ns + nk > 0 {
                        tracing::warn!(
                            "credential rotation: revoked {} web session(s) and {} API key(s) issued with forgeable pre-CSPRNG tokens",
                            ns,
                            nk
                        );
                    }
                }
                _ => {
                    let _ = conn.execute_batch("ROLLBACK");
                    tracing::warn!("credential rotation failed; will retry on next start");
                }
            }
        }

        // Cross-cutting moderation (multibot foundation Step 4). Purely
        // additive: CREATE TABLE/INDEX IF NOT EXISTS never rewrites existing
        // rows, so a legacy install keeps all data and gains empty policy
        // tables. schema.sql declares the same shapes for fresh databases;
        // these statements cover databases created before this feature.
        // Declared after `bots` (above) so the bot_id FKs resolve.
        //
        // Unlike the other best-effort statements in this migration, a
        // moderation DDL failure is NOT silent: the helper logs the failing
        // object + error and we record readiness so policy resolution fails
        // closed instead of returning an empty, permissive policy.
        match Self::migrate_moderation_tables(&conn) {
            Ok(()) => self.moderation_storage_ready.store(true, Ordering::Relaxed),
            Err(()) => self
                .moderation_storage_ready
                .store(false, Ordering::Relaxed),
        }
        Ok(())
    }

    /// Money-safety additive migration (spec §1.1/§1.4, §3, §4, §5).
    ///
    /// Unlike `migrate` this is fallible and `Db::new` propagates the error:
    /// a legacy DB holding more than one in-flight refund for an invoice
    /// fails closed, listing the invoice ids, until an owner voids the
    /// duplicates through the audited Decision-5 path. No money row is ever
    /// deleted or merged (G8). Everything else is `IF NOT EXISTS` /
    /// probe-then-ALTER, so a second open is byte-identical.
    fn migrate_money_safety(&self) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        // 1. Additive columns. Fresh DBs already declare these in schema.sql;
        //    the probe keeps the fresh path free of redundant ALTERs. The
        //    DEFAULT only exists so SQLite can add NOT NULL columns to a
        //    legacy table that already holds rows (§1.1).
        const COLUMN_DDLS: &[(&str, &str, &str)] = &[
            (
                "refunds",
                "idem_key",
                "ALTER TABLE refunds ADD COLUMN idem_key TEXT NOT NULL DEFAULT ''",
            ),
            (
                "refunds",
                "created_by",
                "ALTER TABLE refunds ADD COLUMN created_by INTEGER NOT NULL DEFAULT -1",
            ),
            (
                "refunds",
                "created_role",
                "ALTER TABLE refunds ADD COLUMN created_role TEXT NOT NULL DEFAULT 'unknown'",
            ),
            (
                "refunds",
                "executing_since",
                "ALTER TABLE refunds ADD COLUMN executing_since TEXT",
            ),
            (
                "refunds",
                "reconciliation_since",
                "ALTER TABLE refunds ADD COLUMN reconciliation_since TEXT",
            ),
            (
                "refunds",
                "resolution",
                "ALTER TABLE refunds ADD COLUMN resolution TEXT",
            ),
            (
                "refunds",
                "resolved_by",
                "ALTER TABLE refunds ADD COLUMN resolved_by INTEGER",
            ),
            (
                "refunds",
                "resolved_at",
                "ALTER TABLE refunds ADD COLUMN resolved_at TEXT",
            ),
            (
                "payouts",
                "executing_since",
                "ALTER TABLE payouts ADD COLUMN executing_since TEXT",
            ),
            (
                "payouts",
                "reconciliation_since",
                "ALTER TABLE payouts ADD COLUMN reconciliation_since TEXT",
            ),
            (
                "payouts",
                "resolution",
                "ALTER TABLE payouts ADD COLUMN resolution TEXT",
            ),
            (
                "payouts",
                "resolved_by",
                "ALTER TABLE payouts ADD COLUMN resolved_by INTEGER",
            ),
            (
                "payouts",
                "resolved_at",
                "ALTER TABLE payouts ADD COLUMN resolved_at TEXT",
            ),
            (
                "invoices",
                "paid_ungranted_at",
                "ALTER TABLE invoices ADD COLUMN paid_ungranted_at TEXT",
            ),
        ];
        for (table, column, ddl) in COLUMN_DDLS {
            // `table` comes from the static table above, never from user input.
            let probe = format!("SELECT 1 FROM pragma_table_info('{table}') WHERE name=?1");
            let exists = conn
                .query_row(&probe, params![column], |_| Ok::<_, rusqlite::Error>(1))
                .optional()?;
            if exists.is_none() {
                conn.execute(ddl, [])?;
            }
        }

        // 2. Backfill legacy rows. `idem_key = 'legacy:' || id` is unique by
        //    PK (§1.4); actors come from the refund_create audit row when one
        //    exists, otherwise the documented -1/'unknown' sentinels — never
        //    guessed. Fresh rows never match these predicates.
        conn.execute_batch(
            "UPDATE refunds SET idem_key = 'legacy:' || id
              WHERE idem_key IS NULL OR idem_key = '';
             UPDATE refunds SET created_by = (
                SELECT a.actor_id FROM audit_log a
                WHERE a.action='refund_create' AND a.target_type='refund'
                  AND a.target_id = CAST(refunds.id AS TEXT)
                ORDER BY a.id LIMIT 1
             )
              WHERE (created_by IS NULL OR created_by = -1)
                AND EXISTS (
                   SELECT 1 FROM audit_log a
                   WHERE a.action='refund_create' AND a.target_type='refund'
                     AND a.target_id = CAST(refunds.id AS TEXT)
                     AND a.actor_id IS NOT NULL AND a.actor_id <> -1
                );
             UPDATE refunds SET created_role = (
                SELECT a.actor_role FROM audit_log a
                WHERE a.action='refund_create' AND a.target_type='refund'
                  AND a.target_id = CAST(refunds.id AS TEXT)
                ORDER BY a.id LIMIT 1
             )
              WHERE (created_role IS NULL OR created_role = 'unknown')
                AND EXISTS (
                   SELECT 1 FROM audit_log a
                   WHERE a.action='refund_create' AND a.target_type='refund'
                     AND a.target_id = CAST(refunds.id AS TEXT)
                     AND a.actor_role IS NOT NULL AND a.actor_role <> 'unknown'
                );",
        )?;

        // 3. Migration preflight (§1.4): fail closed on duplicate in-flight
        //    refunds. NEVER delete or merge money rows to make the index fit.
        let duplicates: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT invoice_id, COUNT(*) AS n FROM refunds
                 WHERE status IN ('pending','approved','executing','needs_reconciliation')
                   AND invoice_id IS NOT NULL
                 GROUP BY invoice_id HAVING n > 1 ORDER BY invoice_id",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(format!(
                    "{} ({} in-flight rows)",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?
                ))
            })?;
            let mut found = Vec::new();
            for row in rows {
                found.push(row?);
            }
            found
        };
        if !duplicates.is_empty() {
            return Err(Self::sqlite_fail(format!(
                "money-safety migration refused: multiple in-flight refunds for invoice(s): {}; \
                 resolve each to a single survivor via the audited Decision-5 void path, then reopen",
                duplicates.join(", ")
            )));
        }

        // 4. Invariants. Declared here (not in schema.sql) because the
        //    indexes/triggers reference columns a legacy DB only gains above;
        //    init_schema runs before this method.
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_refunds_invoice_inflight
                ON refunds(invoice_id)
                WHERE status IN ('pending','approved','executing','needs_reconciliation');
             CREATE UNIQUE INDEX IF NOT EXISTS uq_refunds_invoice_idem
                ON refunds(invoice_id, idem_key);
             CREATE TRIGGER IF NOT EXISTS trg_refund_asset_matches_invoice
             BEFORE INSERT ON refunds
             WHEN NEW.invoice_id IS NOT NULL
              AND NEW.asset IS NOT (SELECT asset FROM invoices WHERE invoice_id = NEW.invoice_id)
             BEGIN SELECT RAISE(ABORT, 'refund_asset_mismatch'); END;
             CREATE TRIGGER IF NOT EXISTS trg_refund_money_immutable
             BEFORE UPDATE ON refunds
             WHEN OLD.seller_id   IS NOT NEW.seller_id
               OR OLD.invoice_id  IS NOT NEW.invoice_id
               OR OLD.user_id     IS NOT NEW.user_id
               OR OLD.amount      IS NOT NEW.amount
               OR OLD.asset       IS NOT NEW.asset
               OR OLD.reason      IS NOT NEW.reason
               OR OLD.idem_key    IS NOT NEW.idem_key
               OR OLD.created_by  IS NOT NEW.created_by
               OR OLD.created_role IS NOT NEW.created_role
             BEGIN SELECT RAISE(ABORT, 'refund_row_immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_payout_money_immutable
             BEFORE UPDATE ON payouts
             WHEN OLD.seller_id   IS NOT NEW.seller_id
               OR OLD.amount      IS NOT NEW.amount
               OR OLD.asset       IS NOT NEW.asset
               OR OLD.destination IS NOT NEW.destination
             BEGIN SELECT RAISE(ABORT, 'payout_row_immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_refund_resolution_append_only
             BEFORE UPDATE ON refunds
             WHEN OLD.resolution IS NOT NULL AND NEW.resolution IS NOT OLD.resolution
             BEGIN SELECT RAISE(ABORT, 'refund_resolution_immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_payout_resolution_append_only
             BEFORE UPDATE ON payouts
             WHEN OLD.resolution IS NOT NULL AND NEW.resolution IS NOT OLD.resolution
             BEGIN SELECT RAISE(ABORT, 'payout_resolution_immutable'); END;
             CREATE TRIGGER IF NOT EXISTS trg_invoice_no_failed_after_paid
             BEFORE UPDATE ON invoices
             FOR EACH ROW
             WHEN OLD.status IN ('paid','paid_ungranted') AND NEW.status = 'failed'
             BEGIN SELECT RAISE(ABORT, 'invoice_failed_after_paid_blocked'); END;",
        )?;
        Ok(())
    }

    /// Create every moderation table/index for databases predating the
    /// feature. Each statement is `IF NOT EXISTS`, so this is idempotent and
    /// additive. Any failure is logged with the object name and error (no
    /// secret is logged — these are static DDL statements) and short-circuits
    /// with `Err(())`; the caller flips `moderation_storage_ready` to false so
    /// enforcement fails closed rather than silently yielding an empty policy.
    fn migrate_moderation_tables(conn: &Connection) -> Result<(), ()> {
        const STATEMENTS: &[(&str, &str)] = &[
            (
                "moderation_global_floor",
                "CREATE TABLE IF NOT EXISTS moderation_global_floor (id INTEGER PRIMARY KEY AUTOINCREMENT, term TEXT NOT NULL UNIQUE, mode TEXT NOT NULL CHECK (mode IN ('hide','warn','review')), created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            ),
            (
                "moderation_blocked_words",
                "CREATE TABLE IF NOT EXISTS moderation_blocked_words (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, term TEXT NOT NULL, mode TEXT NOT NULL DEFAULT 'warn' CHECK (mode IN ('hide','warn','review')), enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE (seller_id, term))",
            ),
            (
                "idx_mod_blocked_seller",
                "CREATE INDEX IF NOT EXISTS idx_mod_blocked_seller ON moderation_blocked_words(seller_id)",
            ),
            (
                "moderation_allow_words",
                "CREATE TABLE IF NOT EXISTS moderation_allow_words (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, term TEXT NOT NULL, created_at TEXT NOT NULL, UNIQUE (seller_id, term))",
            ),
            (
                "idx_mod_allow_seller",
                "CREATE INDEX IF NOT EXISTS idx_mod_allow_seller ON moderation_allow_words(seller_id)",
            ),
            (
                "moderation_bot_overrides",
                "CREATE TABLE IF NOT EXISTS moderation_bot_overrides (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, bot_id INTEGER NOT NULL REFERENCES bots(id) ON DELETE CASCADE, term TEXT NOT NULL DEFAULT '', mode TEXT CHECK (mode IN ('hide','warn','review')), inherits INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, UNIQUE (seller_id, bot_id, term), CHECK ((term = '' AND mode IS NULL) OR (term <> '' AND mode IS NOT NULL)))",
            ),
            (
                "idx_mod_bot_override_scope",
                "CREATE INDEX IF NOT EXISTS idx_mod_bot_override_scope ON moderation_bot_overrides(seller_id, bot_id)",
            ),
            (
                "moderation_decisions",
                "CREATE TABLE IF NOT EXISTS moderation_decisions (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, bot_id INTEGER REFERENCES bots(id) ON DELETE SET NULL, term TEXT, action TEXT NOT NULL, reason TEXT, message_ref TEXT, created_at TEXT NOT NULL)",
            ),
            (
                "idx_mod_decisions_seller",
                "CREATE INDEX IF NOT EXISTS idx_mod_decisions_seller ON moderation_decisions(seller_id, created_at)",
            ),
            (
                "idx_mod_decisions_bot",
                "CREATE INDEX IF NOT EXISTS idx_mod_decisions_bot ON moderation_decisions(seller_id, bot_id)",
            ),
            (
                "moderation_appeals",
                "CREATE TABLE IF NOT EXISTS moderation_appeals (id INTEGER PRIMARY KEY AUTOINCREMENT, seller_id INTEGER NOT NULL, bot_id INTEGER REFERENCES bots(id) ON DELETE SET NULL, decision_id INTEGER REFERENCES moderation_decisions(id) ON DELETE SET NULL, message_ref TEXT NOT NULL, reason TEXT, status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','upheld')), created_at TEXT NOT NULL, updated_at TEXT NOT NULL)",
            ),
            (
                "idx_mod_appeals_seller",
                "CREATE INDEX IF NOT EXISTS idx_mod_appeals_seller ON moderation_appeals(seller_id, status)",
            ),
            (
                "idx_mod_appeals_bot",
                "CREATE INDEX IF NOT EXISTS idx_mod_appeals_bot ON moderation_appeals(seller_id, bot_id)",
            ),
        ];
        for (name, sql) in STATEMENTS {
            if let Err(e) = conn.execute(sql, []) {
                tracing::warn!("moderation DDL failed for {}: {}", name, e);
                return Err(());
            }
        }

        // security LOW: make appeal submission idempotent in storage rather than
        // relying on the handler's check-then-insert. Collapse any legacy
        // duplicate tuples *before* the unique index is created, keeping the
        // oldest row per (seller, bot, ref). This mirrors the cleanup in
        // schema.sql (which runs earlier in init_schema) so an upgrade of a
        // DB already holding duplicates never fails to open; the second pass
        // here is idempotent and also covers an index-less DB.
        if let Err(e) = Self::dedup_moderation_appeals(conn) {
            tracing::warn!("moderation appeal dedup failed: {}", e);
            return Err(());
        }
        if let Err(e) = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_mod_appeals_dedup
             ON moderation_appeals(seller_id, COALESCE(bot_id, 0), message_ref)",
            [],
        ) {
            tracing::warn!("moderation DDL failed for idx_mod_appeals_dedup: {}", e);
            return Err(());
        }
        Ok(())
    }

    /// Delete later duplicates of the same appeal tuple, keeping the oldest
    /// (lowest `id`) row. `COALESCE(bot_id, 0)` matches the unique index so
    /// non-null and bot-less submissions are deduped by the same rule; bot ids
    /// are AUTOINCREMENT, so 0 is never a real bot. Purely additive/idempotent:
    /// a second run deletes nothing. Parameterized SQL only, no data loss of
    /// distinct (seller, bot, ref) tuples, and no message text is read/logged.
    fn dedup_moderation_appeals(conn: &Connection) -> Result<(), rusqlite::Error> {
        conn.execute(
            "DELETE FROM moderation_appeals
             WHERE id IN (
                 SELECT d.id FROM moderation_appeals AS d
                 WHERE EXISTS (
                     SELECT 1 FROM moderation_appeals AS k
                     WHERE k.seller_id = d.seller_id
                       AND COALESCE(k.bot_id, 0) = COALESCE(d.bot_id, 0)
                       AND k.message_ref = d.message_ref
                       AND k.id < d.id
                 )
             )",
            [],
        )?;
        Ok(())
    }

    /// Fold the WAL back into the main database file
    /// (PRAGMA wal_checkpoint(TRUNCATE)). Called once on the shutdown path so
    /// backups / file copies see a complete database instead of a main file
    /// plus a trailing -wal log. Best-effort by contract: errors are logged,
    /// never panicked — a checkpoint failure must not turn shutdown into a
    /// crash (the next startup replays the WAL regardless).
    pub fn checkpoint(&self) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // wal_checkpoint always returns exactly one (busy, log, checkpointed)
        // row, so query_row is the deterministic way to run it; execute_batch
        // would swallow the row.
        if let Err(e) = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())) {
            tracing::warn!("WAL checkpoint failed (non-fatal): {}", e);
        }
    }

    /// Read a config value. For non-secret keys behavior is unchanged.
    /// Encrypted values are transparently decoded so legacy readers of a
    /// secret key keep working; a missing key or a corrupt envelope reads as
    /// absent (fail closed) and is logged without any secret material.
    pub fn get_config(&self, key: &str) -> Option<String> {
        let mk = crate::config::master_key().ok().flatten();
        self.get_config_with_key(key, mk.as_ref())
    }

    /// Core of `get_config` with an explicitly resolved key, so the
    /// transparent-read behavior is testable without process environment.
    fn get_config_with_key(&self, key: &str, mk: Option<&MasterKey>) -> Option<String> {
        let raw = self.config_raw(key)?;
        if !crate::secrets::is_encrypted(&raw) {
            return Some(raw);
        }
        let Some(mk) = mk else {
            tracing::warn!(
                "config value for '{}' is encrypted but MASTER_KEY is unavailable",
                key
            );
            return None;
        };
        match decrypt_secret(mk, &raw) {
            Ok(plain) => String::from_utf8(plain).ok(),
            Err(_) => {
                tracing::warn!("config value for '{}' could not be decoded", key);
                None
            }
        }
    }

    pub fn set_config(&self, key: &str, value: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO config(key,value,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![key, value, now],
        );
    }

    // ---- Secrets at rest ----

    /// Config keys that must never be written verbatim. Kept as a list so a
    /// future sensitive key is added in one place; callers still opt in by
    /// using the secret setter rather than `set_config`.
    pub fn is_sensitive_config_key(key: &str) -> bool {
        matches!(key, "crypto_token")
    }

    fn config_raw(&self, key: &str) -> Option<String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Write a sensitive config value as an encrypted envelope. Refuses to
    /// write plaintext when MASTER_KEY is absent or malformed — an operator
    /// must configure the key before secrets can be saved.
    pub fn try_set_secret_config(&self, key: &str, value: &str) -> Result<(), SecretError> {
        if !Self::is_sensitive_config_key(key) {
            return Err(SecretError::NotSensitiveKey);
        }
        let mk = crate::config::master_key()?;
        self.set_secret_config_with(mk.as_ref(), key, value)
    }

    /// Same as `try_set_secret_config` with an explicitly resolved key.
    /// `None` models an absent MASTER_KEY and refuses the write.
    pub(crate) fn set_secret_config_with(
        &self,
        mk: Option<&MasterKey>,
        key: &str,
        value: &str,
    ) -> Result<(), SecretError> {
        if !Self::is_sensitive_config_key(key) {
            return Err(SecretError::NotSensitiveKey);
        }
        let mk = mk.ok_or(SecretError::MissingMasterKey)?;
        let sealed = encrypt_secret(mk, value.as_bytes())?;
        self.set_config(key, &sealed);
        Ok(())
    }

    /// Read a sensitive config value. Legacy unprefixed values are returned
    /// verbatim so existing deployments keep working; encrypted values need a
    /// valid MASTER_KEY and fail closed on tampering or a wrong key.
    pub fn get_secret_config(&self, key: &str) -> Result<Option<String>, SecretError> {
        if !Self::is_sensitive_config_key(key) {
            return Err(SecretError::NotSensitiveKey);
        }
        let Some(raw) = self.config_raw(key) else {
            return Ok(None);
        };
        self.decode_secret_value(&raw)
    }

    pub(crate) fn get_secret_config_with(
        &self,
        mk: &MasterKey,
        key: &str,
    ) -> Result<Option<String>, SecretError> {
        let Some(raw) = self.config_raw(key) else {
            return Ok(None);
        };
        if !crate::secrets::is_encrypted(&raw) {
            return Ok(Some(raw));
        }
        let plain = decrypt_secret(mk, &raw)?;
        Ok(Some(
            String::from_utf8(plain).map_err(|_| SecretError::InvalidUtf8)?,
        ))
    }

    fn decode_secret_value(&self, raw: &str) -> Result<Option<String>, SecretError> {
        if !crate::secrets::is_encrypted(raw) {
            return Ok(Some(raw.to_string()));
        }
        let mk = crate::config::master_key()?.ok_or(SecretError::MissingMasterKey)?;
        let plain = decrypt_secret(&mk, raw)?;
        Ok(Some(
            String::from_utf8(plain).map_err(|_| SecretError::InvalidUtf8)?,
        ))
    }

    // ---- Sellers ----
    pub fn upsert_seller(
        &self,
        user_id: i64,
        username: Option<&str>,
        default_channel: Option<&str>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO sellers(user_id,username,default_channel,created_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(user_id) DO UPDATE SET username=excluded.username, default_channel=COALESCE(excluded.default_channel, sellers.default_channel)",
            params![user_id, username, default_channel, now],
        );
    }

    pub fn get_seller(&self, user_id: i64) -> Option<(i64, Option<String>, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT user_id, username, default_channel FROM sellers WHERE user_id = ?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn seller_channel(&self, seller_id: i64) -> Option<String> {
        self.get_seller(seller_id).and_then(|s| s.2)
    }

    /// Whether the seller has a usable delivery channel configured (their
    /// `sellers.default_channel` exists and is non-empty — mirrors the
    /// `seller_channel(..).unwrap_or_default().is_empty()` check callers do).
    /// Lets code-redeem paths validate BEFORE redeem_code burns a use: the
    /// redeem handler checks this first and refuses the code untouched when
    /// the seller has no channel to grant into.
    pub fn seller_has_channel(&self, seller_id: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT 1 FROM sellers WHERE user_id=?1 AND default_channel IS NOT NULL AND default_channel <> ''",
            params![seller_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// Resolve a seller's user_id from their Telegram username (case-insensitive,
    /// leading '@' tolerated by the caller). Returns None when no seller row
    /// matches.
    pub fn get_seller_by_username(&self, username: &str) -> Option<i64> {
        let uname = username.trim().trim_start_matches('@');
        if uname.is_empty() {
            return None;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT user_id FROM sellers WHERE username = ?1 COLLATE NOCASE",
            params![uname],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn set_seller_channel(&self, seller_id: i64, channel: &str) {
        self.upsert_seller(seller_id, None, Some(channel));
    }

    pub fn list_sellers(&self) -> Vec<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT user_id FROM sellers") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- Posts ----
    /// Record a sent post. `sender` is the sender audit value: `'bot'` for
    /// every Bot-API path, `'user'` when the operator's personal identity
    /// sent it (user-identity design §1). Bot paths pass "bot"; the value is
    /// stored verbatim, so callers own the honesty of the audit column.
    pub fn log_post(
        &self,
        seller_id: i64,
        chat_id: &str,
        message_id: i32,
        kind: &str,
        content: Option<&str>,
        sender: &str,
    ) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO posts(seller_id,chat_id,message_id,kind,content,created_at,sender) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![seller_id, chat_id, message_id, kind, content, now, sender],
        )?;
        Ok(())
    }

    pub fn list_posts(&self, seller_scope: Option<i64>, limit: i64) -> Vec<PostRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT id,seller_id,chat_id,message_id,kind,content,created_at,sender FROM posts WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2".to_string(),
             vec![Box::new(sid), Box::new(limit)])
        } else {
            ("SELECT id,seller_id,chat_id,message_id,kind,content,created_at,sender FROM posts ORDER BY id DESC LIMIT ?1".to_string(),
             vec![Box::new(limit)])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i32>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Owner of a post, for authorization checks before the unscoped
    /// `delete_post` / `update_post_content` mutations. `None` means the post
    /// does not exist (or the read failed), so callers must deny by default.
    pub fn get_post_seller(&self, id: i64) -> Option<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT seller_id FROM posts WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn delete_post(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute("DELETE FROM posts WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn update_post_content(&self, id: i64, content: &str) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE posts SET content=?1 WHERE id=?2",
            params![content, id],
        )?;
        Ok(())
    }

    // ---- AI post drafts ----
    pub fn create_ai_post_draft(&self, seller_id: i64, channel: &str, content: &str) -> i64 {
        self.create_ai_post_draft_with_template(seller_id, channel, None, content)
    }

    pub fn create_ai_post_draft_with_template(
        &self,
        seller_id: i64,
        channel: &str,
        template: Option<&str>,
        content: &str,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO ai_post_drafts(seller_id,channel,template,content,status,created_at,updated_at) VALUES(?1,?2,?3,?4,'draft',?5,?5)",
            params![seller_id, channel, template, content, now],
        );
        conn.last_insert_rowid()
    }

    // Aliases for create.
    pub fn create_ai_draft(&self, seller_id: i64, channel: &str, content: &str) -> i64 {
        self.create_ai_post_draft(seller_id, channel, content)
    }
    pub fn create_draft(&self, seller_id: i64, channel: &str, content: &str) -> i64 {
        self.create_ai_post_draft(seller_id, channel, content)
    }
    pub fn add_ai_post_draft(&self, seller_id: i64, channel: &str, content: &str) -> i64 {
        self.create_ai_post_draft(seller_id, channel, content)
    }

    pub fn get_ai_post_draft(
        &self,
        id: i64,
    ) -> Option<(
        i64,
        i64,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
        Option<i64>,
    )> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,channel,template,content,status,created_at,updated_at,published_message_id FROM ai_post_drafts WHERE id=?1",
            params![id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    // Aliases for get.
    pub fn get_ai_draft(
        &self,
        id: i64,
    ) -> Option<(
        i64,
        i64,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
        Option<i64>,
    )> {
        self.get_ai_post_draft(id)
    }
    pub fn get_draft(
        &self,
        id: i64,
    ) -> Option<(
        i64,
        i64,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
        Option<i64>,
    )> {
        self.get_ai_post_draft(id)
    }

    pub fn update_ai_post_draft(&self, id: i64, content: &str) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE ai_post_drafts SET content=?1, updated_at=?2 WHERE id=?3",
            params![content, now, id],
        )?;
        Ok(())
    }

    // Aliases for update.
    pub fn update_ai_post_draft_content(
        &self,
        id: i64,
        content: &str,
    ) -> Result<(), rusqlite::Error> {
        self.update_ai_post_draft(id, content)
    }
    pub fn update_ai_draft(&self, id: i64, content: &str) -> Result<(), rusqlite::Error> {
        self.update_ai_post_draft(id, content)
    }
    pub fn update_draft(&self, id: i64, content: &str) -> Result<(), rusqlite::Error> {
        self.update_ai_post_draft(id, content)
    }

    pub fn mark_ai_post_draft_published(
        &self,
        id: i64,
        message_id: i64,
    ) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE ai_post_drafts SET status='published', published_message_id=?1, updated_at=?2 WHERE id=?3",
            params![message_id, now, id],
        )?;
        Ok(())
    }

    // Aliases for mark published.
    pub fn mark_ai_post_published(&self, id: i64, message_id: i64) -> Result<(), rusqlite::Error> {
        self.mark_ai_post_draft_published(id, message_id)
    }
    pub fn mark_draft_published(&self, id: i64, message_id: i64) -> Result<(), rusqlite::Error> {
        self.mark_ai_post_draft_published(id, message_id)
    }
    pub fn mark_published(&self, id: i64, message_id: i64) -> Result<(), rusqlite::Error> {
        self.mark_ai_post_draft_published(id, message_id)
    }

    pub fn delete_ai_post_draft(&self, id: i64) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute("DELETE FROM ai_post_drafts WHERE id=?1", params![id])?;
        Ok(())
    }

    // Aliases for delete.
    pub fn delete_ai_draft(&self, id: i64) -> Result<(), rusqlite::Error> {
        self.delete_ai_post_draft(id)
    }
    pub fn delete_draft(&self, id: i64) -> Result<(), rusqlite::Error> {
        self.delete_ai_post_draft(id)
    }

    // ---- Scheduled posts ----
    pub fn create_scheduled_post(
        &self,
        seller_id: impl Into<i64>,
        chat_id: impl AsRef<str>,
        content: impl AsRef<str>,
        kind: impl AsRef<str>,
        scheduled_at: impl AsRef<str>,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO scheduled_posts(seller_id,chat_id,content,kind,scheduled_at,status,created_at) VALUES(?1,?2,?3,?4,?5,'pending',?6)",
            params![
                seller_id.into(),
                chat_id.as_ref(),
                content.as_ref(),
                kind.as_ref(),
                scheduled_at.as_ref(),
                now
            ],
        );
        conn.last_insert_rowid()
    }

    pub fn list_due_scheduled(&self) -> Vec<ScheduledPostRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,chat_id,content,kind,scheduled_at,status,created_at,posted_message_id FROM scheduled_posts WHERE status='pending' AND scheduled_at <= ?1 ORDER BY scheduled_at",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![now], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn mark_scheduled_posted(
        &self,
        id: impl Into<i64>,
        message_id: impl std::fmt::Display,
    ) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mid: i64 = message_id.to_string().parse().unwrap_or(0);
        conn.execute(
            "UPDATE scheduled_posts SET status='posted', posted_message_id=?2 WHERE id=?1",
            params![id.into(), mid],
        )?;
        Ok(())
    }

    /// Bump and return the send-attempts counter of a scheduled post. Used by
    /// the sender loop to bound retries: at the cap the post is marked
    /// 'failed' (terminal — list_due_scheduled only returns 'pending').
    /// Returns 0 on any error, which reads as "not capped": a degraded
    /// counter degrades to today's retry-forever behavior, never to a
    /// spurious failure.
    pub fn bump_scheduled_attempts(&self, id: i64) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "UPDATE scheduled_posts SET attempts = attempts + 1 WHERE id = ?1 RETURNING attempts",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    /// Terminal-fail a scheduled post after the retry cap. Scoped to
    /// status='pending' so a concurrently-posted row is never overwritten.
    pub fn mark_scheduled_failed(&self, id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Err(e) = conn.execute(
            "UPDATE scheduled_posts SET status='failed' WHERE id=?1 AND status='pending'",
            params![id],
        ) {
            tracing::warn!("failed to mark scheduled post {} as failed: {}", id, e);
        }
    }

    /// Pending scheduled posts, optionally scoped to one seller (None = all —
    /// the admin/owner view). Soonest-first for the bot's /scheduled listing;
    /// 'cancelled'/'posted'/'failed' rows never appear (status='pending' only).
    pub fn list_pending_scheduled_posts(&self, seller_scope: Option<i64>) -> Vec<ScheduledPostRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT id,seller_id,chat_id,content,kind,scheduled_at,status,created_at,posted_message_id FROM scheduled_posts WHERE status='pending' AND seller_id=?1 ORDER BY scheduled_at, id"
                    .to_string(),
                vec![Box::new(sid)],
            )
        } else {
            (
                "SELECT id,seller_id,chat_id,content,kind,scheduled_at,status,created_at,posted_message_id FROM scheduled_posts WHERE status='pending' ORDER BY scheduled_at, id"
                    .to_string(),
                vec![],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Cancel one pending scheduled post. `seller_scope = Some(sid)` cancels
    /// only that seller's row (a seller can never cancel another seller's
    /// post); `None` (admin/owner) cancels any pending row. Returns false for
    /// unknown ids and rows already posted/failed/cancelled — the caller
    /// answers one refusal for both, so there is no state oracle. Scoped to
    /// status='pending' like mark_scheduled_failed: a concurrently-posted row
    /// is never overwritten.
    pub fn cancel_scheduled_post(&self, id: i64, seller_scope: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE scheduled_posts SET status='cancelled' WHERE id=?1 AND status='pending' AND (?2 IS NULL OR seller_id=?2)",
            params![id, seller_scope],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Pending scheduled posts whose stored `scheduled_at` cannot parse as
    /// RFC3339. The due queue compares the column lexicographically, so such
    /// rows either fire at the wrong time (a string sorting below an RFC3339
    /// timestamp) or sit pending forever (one sorting above). The sender loop
    /// sweeps both into the bounded terminal-failed path; parsing happens in
    /// Rust so the check matches exactly what the sender can honor.
    pub fn list_pending_scheduled_unparseable(&self) -> Vec<(i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn
            .prepare("SELECT id, scheduled_at FROM scheduled_posts WHERE status='pending'")
        {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .ok();
        match rows {
            Some(iter) => iter
                .filter_map(|r| r.ok())
                .filter(|(_, s)| chrono::DateTime::parse_from_rfc3339(s).is_err())
                .collect(),
            None => vec![],
        }
    }

    pub fn list_scheduled_posts(&self, seller_scope: Option<i64>) -> Vec<ScheduledPostRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT id,seller_id,chat_id,content,kind,scheduled_at,status,created_at,posted_message_id FROM scheduled_posts WHERE seller_id=?1 ORDER BY id DESC"
                    .to_string(),
                vec![Box::new(sid)],
            )
        } else {
            (
                "SELECT id,seller_id,chat_id,content,kind,scheduled_at,status,created_at,posted_message_id FROM scheduled_posts ORDER BY id DESC"
                    .to_string(),
                vec![],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, String>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_user(&self, user_id: i64) -> Option<(i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT user_id, username FROM users WHERE user_id=?1",
            params![user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    // ---- Invoices ----
    #[allow(clippy::too_many_arguments)]
    pub fn save_invoice(
        &self,
        invoice_id: &str,
        seller_id: i64,
        amount: &str,
        asset: &str,
        description: Option<&str>,
        status: &str,
        pay_url: Option<&str>,
        plan_id: Option<&str>,
        user_id: Option<i64>,
        product_id: Option<i64>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO invoices(invoice_id,seller_id,amount,asset,description,status,pay_url,plan_id,user_id,product_id,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?11)
             ON CONFLICT(invoice_id) DO UPDATE SET status=excluded.status, pay_url=excluded.pay_url, plan_id=excluded.plan_id, user_id=excluded.user_id, product_id=excluded.product_id, updated_at=excluded.updated_at",
            params![invoice_id, seller_id, amount, asset, description, status, pay_url, plan_id, user_id, product_id, now],
        );
    }

    pub fn update_invoice_status(&self, invoice_id: &str, status: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE invoices SET status=?1, updated_at=?2 WHERE invoice_id=?3",
            params![status, now, invoice_id],
        );
    }

    /// Bump and return the verification-attempts counter of an invoice. The
    /// counter advances ONLY on verification errors (no token configured,
    /// provider error, unresolvable channel) — the "not settled yet" release
    /// does not count, so a buyer who pays days later is never aged out. At
    /// the cap the caller marks the invoice terminal-failed ('failed':
    /// pending_invoices() filters it out; reversible only by an operator via
    /// the admin invoice-status API). Returns 0 on any error, which reads as
    /// "not capped": a degraded counter degrades to retry-forever behavior,
    /// never to a spurious terminal failure.
    pub fn bump_invoice_attempts(&self, invoice_id: &str) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.query_row(
            "UPDATE invoices SET attempts = attempts + 1, updated_at=?2 WHERE invoice_id = ?1 RETURNING attempts",
            params![invoice_id, now],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    pub fn get_invoice(&self, invoice_id: &str) -> Option<InvoiceRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT amount,asset,status,COALESCE(description,''),plan_id,user_id,seller_id FROM invoices WHERE invoice_id=?1",
            params![invoice_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional().ok().flatten()
    }

    /// Product bound to an invoice (NULL = plan path). Reads the v1.2
    /// `product_id` column added by migrate().
    pub fn invoice_product(&self, invoice_id: &str) -> Option<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT product_id FROM invoices WHERE invoice_id=?1",
            params![invoice_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Delivery payload for a product invoice:
    /// (product_id, kind, name, delivery) -- None for plan invoices.
    pub fn invoice_product_delivery(
        &self,
        invoice_id: &str,
    ) -> Option<(i64, String, String, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT p.id, p.kind, p.name, p.delivery FROM invoices i JOIN products p ON p.id = i.product_id WHERE i.invoice_id=?1",
            params![invoice_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Atomically claim an invoice for granting: flips 'active' -> 'granting'
    /// only if no other granter got there first. Serializes concurrent
    /// granters at the DB level. The claim stamps `updated_at` so
    /// pending_invoices()'s wedged-grant recovery measures staleness from
    /// claim acquisition: a freshly claimed invoice never looks wedged, and
    /// one still 'granting' after WEDGED_GRANT_STALE_MINUTES is one whose
    /// claimer died mid-grant.
    pub fn claim_invoice_for_grant(&self, invoice_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE invoices SET status='granting', updated_at=?2 WHERE invoice_id=?1 AND status='active'",
            params![invoice_id, now],
        ) == Ok(1)
    }

    /// Release a claim that cannot proceed (e.g. no resolvable seller
    /// channel): flips 'granting' -> 'active' ONLY, never touching rows in
    /// any other state, so it can never resurrect a 'paid'/'failed' invoice
    /// or steal another granter's claim except to undo it. Returns whether a
    /// row was released. This is the ONLY sanctioned way to unwind a claim —
    /// callers must never write the 'granting' status directly (it is set
    /// exclusively by claim_invoice_for_grant, which keeps the active ->
    /// granting handoff atomic and fail-closed).
    pub fn release_invoice_claim(&self, invoice_id: impl ToString) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let inv = invoice_id.to_string();
        conn.execute(
            "UPDATE invoices SET status='active', updated_at=?2 WHERE invoice_id=?1 AND status='granting'",
            params![inv, now],
        ) == Ok(1)
    }

    /// Mark a claimed invoice as fully granted/paid. Best-effort: errors are
    /// logged, not swallowed silently.
    pub fn finalize_invoice_paid(&self, invoice_id: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        if let Err(e) = conn.execute(
            "UPDATE invoices SET status='paid', updated_at=?2 WHERE invoice_id=?1",
            params![invoice_id, now],
        ) {
            tracing::warn!("failed to mark invoice {} as paid: {}", invoice_id, e);
        }
    }

    /// §3.2: server-side verification said `paid` but the exactly-once grant
    /// claim produced no subscription/product grant. CAS from any non-terminal
    /// state into `paid_ungranted`, stamping the recovery clock ONCE (the 24h
    /// and 72h timers are measured from this value, never from a retry).
    /// Returns false for a terminal `refunded`/`failed` row.
    pub fn mark_invoice_paid_ungranted(&self, invoice_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE invoices SET status='paid_ungranted', \
             paid_ungranted_at=COALESCE(paid_ungranted_at, ?2), updated_at=?2 \
             WHERE invoice_id=?1 AND status NOT IN ('refunded','failed')",
            params![invoice_id, now],
        ) == Ok(1)
    }

    /// §3.3 success path: a later exactly-once claim succeeded, so the row
    /// returns to `paid` and leaves the recovery queue. Clears the clock so a
    /// subsequent ungranted event starts a fresh window.
    pub fn finalize_invoice_grant_retry(&self, invoice_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE invoices SET status='paid', paid_ungranted_at=NULL, updated_at=?2 \
             WHERE invoice_id=?1 AND status='paid_ungranted'",
            params![invoice_id, now],
        ) == Ok(1)
    }

    /// The recovery clock of one invoice, if it is (or was) ungranted.
    pub fn invoice_paid_ungranted_at(&self, invoice_id: &str) -> Option<String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT paid_ungranted_at FROM invoices WHERE invoice_id=?1",
            params![invoice_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// §3.3/§3.4 queue: every still-ungranted invoice whose clock is at or
    /// before the injected cutoff. Returns
    /// (invoice_id, seller_id, amount, asset, paid_ungranted_at) — all
    /// seller-scoped and fully parameterized.
    #[allow(clippy::type_complexity)]
    pub fn paid_ungranted_before(
        &self,
        cutoff: &str,
    ) -> Vec<(String, i64, String, String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT invoice_id,seller_id,amount,asset,paid_ungranted_at FROM invoices \
             WHERE status='paid_ungranted' AND paid_ungranted_at IS NOT NULL \
               AND paid_ungranted_at <= ?1 ORDER BY paid_ungranted_at, invoice_id",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(params![cutoff], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        });
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    /// §3.5 blocking-liability check: seller invoices still ungranted past
    /// the escalation cutoff whose auto-refund (`idem_key` is the documented
    /// `auto:ungranted:<invoice_id>`) has NOT reached a terminal executed
    /// state. The server returns 409 `unresolved_ungranted_liability` for these.
    pub fn unresolved_ungranted_invoices(
        &self,
        seller_id: i64,
        escalation_cutoff: &str,
    ) -> Vec<String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT i.invoice_id FROM invoices i \
             WHERE i.seller_id=?1 AND i.status='paid_ungranted' \
               AND i.paid_ungranted_at IS NOT NULL AND i.paid_ungranted_at <= ?2 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM refunds r \
                 WHERE r.invoice_id=i.invoice_id \
                   AND r.idem_key='auto:ungranted:' || i.invoice_id \
                   AND r.status IN ('executed','processed') \
               ) ORDER BY i.invoice_id",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(params![seller_id, escalation_cutoff], |r| {
            r.get::<_, String>(0)
        });
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    pub fn subscription_exists_for_invoice(&self, invoice_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT 1 FROM subscriptions WHERE invoice_id=?1 LIMIT 1",
            params![invoice_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// Invoices still awaiting (or stuck mid-) grant, returned as
    /// (invoice_id, plan_id, user_id, seller_id). Two disjoint queues:
    ///
    /// * every 'active' invoice that carries something grantable — a plan
    ///   (plan_id) OR a product (product_id); product-only invoices were
    ///   previously filtered out here and never auto-processed;
    /// * 'granting' invoices older than WEDGED_GRANT_STALE_MINUTES — crash
    ///   recovery for a claimer that died between claim_invoice_for_grant and
    ///   finalize_invoice_paid. Re-verification is safe to re-run because the
    ///   grant step is idempotent: the product branch is gated by
    ///   product_grant_exists plus grant_product's atomic
    ///   exists-check/decrement/insert (UNIQUE(invoice_id) on
    ///   product_grants), the plan branch by subscription_exists_for_invoice
    ///   plus the one-subscription-per-invoice unique index, and
    ///   finalize_invoice_paid is a status set. INVARIANT: a just-claimed
    ///   invoice is never re-queued, because the claim stamps updated_at and
    ///   only rows older than the threshold requeue — the live claimer owns
    ///   the row exclusively until it finalizes, releases, or dies.
    pub fn pending_invoices(&self) -> Vec<(String, Option<String>, Option<i64>, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cutoff = (Utc::now()
            - chrono::Duration::try_minutes(WEDGED_GRANT_STALE_MINUTES).unwrap_or_default())
        .to_rfc3339();
        let mut stmt = match conn.prepare(
            "SELECT invoice_id, plan_id, user_id, seller_id FROM invoices \
             WHERE (status = 'active' OR (status = 'granting' AND updated_at < ?1)) \
             AND (plan_id IS NOT NULL OR product_id IS NOT NULL)",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![cutoff], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_invoices(
        &self,
        seller_scope: Option<i64>,
        limit: i64,
    ) -> Vec<(String, String, String, String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT invoice_id,amount,asset,status,COALESCE(pay_url,'') FROM invoices WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2".to_string(),
             vec![Box::new(sid), Box::new(limit)])
        } else {
            ("SELECT invoice_id,amount,asset,status,COALESCE(pay_url,'') FROM invoices ORDER BY id DESC LIMIT ?1".to_string(),
             vec![Box::new(limit)])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- Admins ----
    pub fn is_admin(&self, user_id: i64, bot_id: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count: i64 = match bot_id {
            Some(bid) => conn
                .query_row(
                    "SELECT COUNT(*) FROM admins WHERE user_id = ?1 AND (bot_id IS NULL OR bot_id = ?2)",
                    params![user_id, bid],
                    |r| r.get(0),
                )
                .unwrap_or(0),
            None => conn
                .query_row(
                    "SELECT COUNT(*) FROM admins WHERE user_id = ?1 AND bot_id IS NULL",
                    params![user_id],
                    |r| r.get(0),
                )
                .unwrap_or(0),
        };
        count > 0
    }

    /// Bot id assigned to an admin (NULL → None for global).
    pub fn get_admin_bot_id(&self, user_id: i64) -> Option<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT bot_id FROM admins WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Scope an existing admin to a bot (`Some(bot_id)`) or clear the scope
    /// back to a global admin (`None` → NULL). A `Some` id must reference an
    /// existing `bots` row: unknown ids are rejected without touching the row,
    /// so a stale console form can never write a dangling scope. Returns true
    /// iff the UPDATE changed an admins row (unknown user → false). Only the
    /// `bot_id` column moves — `is_admin`/`get_admin_bot_id` semantics are
    /// untouched, so clearing restores global-admin visibility.
    pub fn set_admin_bot_id(&self, user_id: i64, bot_id: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(bid) = bot_id {
            let known = conn
                .query_row("SELECT 1 FROM bots WHERE id = ?1", params![bid], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()
                .ok()
                .flatten()
                .is_some();
            if !known {
                return false;
            }
        }
        conn.execute(
            "UPDATE admins SET bot_id = ?1 WHERE user_id = ?2",
            params![bot_id, user_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    // ---- Bots ----
    /// Legacy constructor: creates a `shop_bot`, preserving the behavior of
    /// every pre-multibot call site byte-for-byte. Typed callers should use
    /// `create_bot_with_type`.
    pub fn create_bot(
        &self,
        key: &str,
        owner_user_id: i64,
        default_channel_id: Option<i64>,
        name: &str,
    ) -> i64 {
        self.create_bot_with_type(
            key,
            owner_user_id,
            default_channel_id,
            name,
            crate::config::DEFAULT_BOT_TYPE,
        )
    }

    /// Create a bot with an explicit product/capability type. All values are
    /// bound parameters; the default type keeps legacy callers on `shop_bot`.
    pub fn create_bot_with_type(
        &self,
        key: &str,
        owner_user_id: i64,
        default_channel_id: Option<i64>,
        name: &str,
        bot_type: &str,
    ) -> i64 {
        let bot_type = crate::config::normalize_bot_type(bot_type);
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = chrono::Utc::now().to_rfc3339();
        let res = conn.execute(
            "INSERT INTO bots(key, name, owner_user_id, default_channel_id, enabled, created_at, bot_type) VALUES(?1,?2,?3,?4,1,?5,?6)",
            rusqlite::params![key, name, owner_user_id, default_channel_id, now, bot_type],
        );
        if res.is_ok() {
            return conn.last_insert_rowid();
        }
        conn.query_row(
            "SELECT id FROM bots WHERE key = ?1",
            rusqlite::params![key],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn get_bot(&self, id: i64) -> Option<BotRecord> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT id, key, username, owner_user_id, default_channel_id, enabled, created_at, name, bot_type FROM bots WHERE id = ?1",
            rusqlite::params![id],
            bot_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn get_bot_by_key(&self, key: &str) -> Option<BotRecord> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT id, key, username, owner_user_id, default_channel_id, enabled, created_at, name, bot_type FROM bots WHERE key = ?1",
            rusqlite::params![key],
            bot_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_bots(&self) -> Vec<BotRecord> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id, key, username, owner_user_id, default_channel_id, enabled, created_at, name, bot_type FROM bots ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map([], bot_from_row)
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_bots_enabled(&self) -> Vec<BotRecord> {
        self.list_bots().into_iter().filter(|b| b.enabled).collect()
    }

    /// List every bot of one product/capability type. The type is a bound
    /// parameter; an unknown type yields an empty vec — never a silent
    /// fallback to all bots (the dispatcher refuses disabled types).
    pub fn list_bots_of_type(&self, bot_type: &str) -> Vec<BotRecord> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id, key, username, owner_user_id, default_channel_id, enabled, created_at, name, bot_type FROM bots WHERE bot_type = ?1 ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![bot_type], bot_from_row)
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Enabled-only variant of `list_bots_of_type`, mirroring
    /// `list_bots_enabled` for callers that dispatch inbound updates.
    pub fn list_bots_of_type_enabled(&self, bot_type: &str) -> Vec<BotRecord> {
        self.list_bots_of_type(bot_type)
            .into_iter()
            .filter(|b| b.enabled)
            .collect()
    }

    /// NULL `bot_id` sellers follow the keyed primary so legacy rows stay
    /// scoped when extra bots are added.
    pub fn list_bots_of_type_for_seller(&self, seller_id: i64, bot_type: &str) -> Vec<BotRecord> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT b.id, b.key, b.username, b.owner_user_id, b.default_channel_id, b.enabled, b.created_at, b.name, b.bot_type FROM bots b JOIN sellers s ON COALESCE(s.bot_id, (SELECT id FROM bots WHERE key = 'primary'), (SELECT MIN(id) FROM bots)) = b.id WHERE s.user_id = ?1 AND b.bot_type = ?2 ORDER BY b.id",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![seller_id, bot_type], bot_from_row)
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// NULL `bot_id` sellers belong to the keyed primary so unassigned rows
    /// remain visible after extra bots are added.
    pub fn sellers_for_bot(&self, bot_id: i64) -> Vec<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT user_id FROM sellers WHERE COALESCE(bot_id, (SELECT id FROM bots WHERE key = 'primary'), (SELECT MIN(id) FROM bots)) = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(rusqlite::params![bot_id], |r| r.get::<_, i64>(0))
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Default channel name for a bot: channels.bot_id + is_default first,
    /// then bots.default_channel_id, then any channel for the bot.
    pub fn default_channel_for_bot(&self, bot_id: i64) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        // 1. default channel row for bot
        if let Ok(ch) = conn.query_row(
            "SELECT channel FROM channels WHERE bot_id = ?1 AND is_default = 1 LIMIT 1",
            rusqlite::params![bot_id],
            |r| r.get::<_, String>(0),
        ) {
            return Some(ch);
        }
        // 2. bots.default_channel_id -> channels
        if let Ok(Some(c)) = conn.query_row(
            "SELECT c.channel FROM bots b LEFT JOIN channels c ON c.id = b.default_channel_id WHERE b.id = ?1",
            rusqlite::params![bot_id],
            |r| r.get::<_, Option<String>>(0),
        ) && !c.trim().is_empty()
        {
            return Some(c);
        }
        // 3. any channel for bot
        conn.query_row(
            "SELECT channel FROM channels WHERE bot_id = ?1 LIMIT 1",
            rusqlite::params![bot_id],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Enable or disable a bot by id (best-effort; ignored on unknown id).
    pub fn set_bot_enabled(&self, id: i64, enabled: bool) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "UPDATE bots SET enabled = ?1 WHERE id = ?2",
            rusqlite::params![if enabled { 1 } else { 0 }, id],
        );
    }

    /// Set a bot's console display name. The name is whitespace-trimmed;
    /// the empty string is allowed and means "show the key instead". No
    /// length cap is enforced here (sibling bot setters validate none) —
    /// the 64-char bound is a console-input concern. Returns an honest Err
    /// for an unknown bot id, mirroring `set_admin_bot_id`'s refusal, so
    /// the console can surface it. Parameterized SQL only.
    pub fn set_bot_name(&self, bot_id: i64, name: &str) -> Result<(), String> {
        let name = name.trim();
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        match conn.execute(
            "UPDATE bots SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, bot_id],
        ) {
            Ok(0) => Err(format!("bot {bot_id} not found")),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("failed to set name for bot {bot_id}: {e}")),
        }
    }

    /// Set a bot's product/capability type. Empty/whitespace input falls
    /// back to `shop_bot` (fail-safe), unknown bot ids are refused, and
    /// failures are reported honestly. Parameterized SQL only.
    pub fn set_bot_type(&self, bot_id: i64, bot_type: &str) -> Result<(), String> {
        let bot_type = crate::config::normalize_bot_type(bot_type);
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        match conn.execute(
            "UPDATE bots SET bot_type = ?1 WHERE id = ?2",
            rusqlite::params![bot_type, bot_id],
        ) {
            Ok(0) => Err(format!("bot {bot_id} not found")),
            Ok(_) => Ok(()),
            Err(e) => Err(format!("failed to set type for bot {bot_id}: {e}")),
        }
    }

    /// Update a bot's owner, default channel and enabled flag in one
    /// statement. Returns whether a row was updated (false for unknown id).
    pub fn update_bot_settings(
        &self,
        id: i64,
        owner_user_id: i64,
        default_channel_id: Option<i64>,
        enabled: bool,
    ) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE bots SET owner_user_id = ?1, default_channel_id = ?2, enabled = ?3 WHERE id = ?4",
            rusqlite::params![owner_user_id, default_channel_id, if enabled { 1 } else { 0 }, id],
        )
        .unwrap_or(0)
            > 0
    }

    /// Delete a bot row by id. Returns true iff a row was deleted. Any
    /// `web_sessions.selected_bot_id` referencing this bot is set to NULL
    /// first so deleting a bot never leaves dangling session selections.
    /// Parameterized SQL throughout; scoped strictly by id.
    pub fn delete_bot(&self, id: i64) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "UPDATE web_sessions SET selected_bot_id = NULL WHERE selected_bot_id = ?1",
            rusqlite::params![id],
        );
        conn.execute("DELETE FROM bots WHERE id = ?1", rusqlite::params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn upsert_seller_bot_id(&self, seller_id: i64, bot_id: Option<i64>) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = chrono::Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO sellers(user_id, created_at, bot_id) VALUES(?1,?2,?3)
             ON CONFLICT(user_id) DO UPDATE SET bot_id = excluded.bot_id",
            rusqlite::params![seller_id, now, bot_id],
        );
    }

    pub fn get_seller_bot_id(&self, seller_id: i64) -> Option<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT bot_id FROM sellers WHERE user_id = ?1",
            rusqlite::params![seller_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    pub fn get_channel_bot_id(&self, channel: &str) -> Option<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT bot_id FROM channels WHERE channel = ?1 COLLATE NOCASE",
            rusqlite::params![channel],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    pub fn get_channel_seller_id(&self, channel: &str) -> Option<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT seller_id FROM channels WHERE channel = ?1 COLLATE NOCASE",
            rusqlite::params![channel],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Per-channel sender binding set by the identity UI: `'bot'` (default)
    /// or `'user'`. `None` when the channel does not exist or the column has
    /// not been migrated yet — `BotManager::for_channel_sender` treats both
    /// as `'bot'` (fail-safe default, never posts as the personal account
    /// without an explicit binding).
    pub fn get_channel_sender_identity(&self, channel: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT sender_identity FROM channels WHERE channel = ?1 COLLATE NOCASE",
            rusqlite::params![channel],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    /// Bind a configured channel to the identity that posts to it
    /// (user-identity design §1): `'bot'` (default) or `'user'` — the
    /// operator's personal account. The identity value is validated HERE so
    /// no caller can store anything outside the two-letter contract;
    /// parameterized SQL throughout. Returns `true` when a channel row was
    /// updated — `false` for an unknown channel or an invalid identity (the
    /// caller surfaces the fix; this method never creates channels).
    pub fn set_channel_sender_identity(&self, channel: &str, identity: &str) -> bool {
        if identity != "bot" && identity != "user" {
            return false;
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE channels SET sender_identity = ?1 WHERE channel = ?2 COLLATE NOCASE",
            rusqlite::params![identity, channel],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Store a human-readable title for a channel row (best-effort; missing
    /// row or empty title is a no-op). Used by the console when a channel is
    /// added so the list can show "Tech Tips (-100…)" instead of a bare id.
    pub fn set_channel_title(&self, channel: &str, title: Option<&str>) {
        let Some(t) = title.map(str::trim).filter(|s| !s.is_empty()) else {
            return;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE channels SET title = ?1 WHERE channel = ?2 COLLATE NOCASE",
            params![t, channel.trim()],
        );
    }

    pub fn get_channel_id(&self, channel: &str) -> Option<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT id FROM channels WHERE channel = ?1 COLLATE NOCASE",
            rusqlite::params![channel],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Canonical name of an explicitly configured channel: exact name match
    /// (the column's NOCASE collation makes it case-insensitive), returning
    /// the name as stored. `None` when no channel row exists. The identity
    /// actor uses this to scope personal-account reads to configured channels
    /// only (user-identity design decision 5) — an unconfigured chat never
    /// reaches MTProto.
    pub fn known_channel(&self, channel: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT channel FROM channels WHERE channel = ?1 COLLATE NOCASE",
            rusqlite::params![channel],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn backfill_bots_at_startup(&self, bots: &[crate::config::BotConfig]) {
        if !self.list_bots().is_empty() {
            return;
        }
        if bots.is_empty() {
            return;
        }
        for b in bots {
            let channel_id = b.default_channel.as_ref().and_then(|ch| {
                let _ = self.add_channel(ch, 0);
                self.get_channel_id(ch)
            });
            self.create_bot_with_type(&b.key, b.owner_id, channel_id, b.name.trim(), &b.bot_type);
        }
        if let Some(primary) = self.list_bots().first() {
            let pid = primary.id;
            let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            let _ = conn.execute(
                "UPDATE sellers SET bot_id = ?1 WHERE bot_id IS NULL",
                rusqlite::params![pid],
            );
            let _ = conn.execute(
                "UPDATE channels SET bot_id = ?1 WHERE bot_id IS NULL",
                rusqlite::params![pid],
            );
            let _ = conn.execute(
                "UPDATE admins SET bot_id = ?1 WHERE bot_id IS NULL",
                rusqlite::params![pid],
            );
        }
    }

    pub fn add_admin(&self, user_id: i64, username: Option<&str>) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT OR IGNORE INTO admins(user_id,username,added_at) VALUES(?1,?2,?3)",
            params![user_id, username, now],
        );
    }

    pub fn remove_admin(&self, user_id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute("DELETE FROM admins WHERE user_id = ?1", params![user_id]);
    }

    pub fn list_admins(&self) -> Vec<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT user_id FROM admins") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_admins_detail(&self) -> Vec<(i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT user_id, username FROM admins ORDER BY user_id") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- Plans ----
    pub fn add_plan(
        &self,
        seller_id: i64,
        name: &str,
        price: &str,
        asset: &str,
        days: i64,
        channel_id: Option<&str>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO plans(seller_id,name,price,asset,duration_days,channel_id,active,created_at) VALUES(?1,?2,?3,?4,?5,?6,1,?7)",
            params![seller_id, name, price, asset, days, channel_id, now],
        );
    }

    pub fn remove_plan(&self, seller_id: i64, name: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "DELETE FROM plans WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
        );
    }

    pub fn list_plans(&self, seller_scope: Option<i64>) -> Vec<PlanRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT name,price,asset,duration_days,channel_id,active,seller_id FROM plans WHERE seller_id=?1 ORDER BY id".to_string(), vec![Box::new(sid)])
        } else {
            ("SELECT name,price,asset,duration_days,channel_id,active,seller_id FROM plans ORDER BY id".to_string(), vec![])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get::<_, i64>(5)? != 0,
                        r.get(6)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_plan(
        &self,
        seller_id: i64,
        name: &str,
    ) -> Option<(i64, String, String, String, i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,price,asset,name,duration_days,channel_id FROM plans WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        ).optional().ok().flatten()
    }

    pub fn get_plan_global(&self, name: &str) -> Option<PlanGlobalRow> {
        // (seller_id, id, price, asset, name, days, channel)
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT seller_id,id,price,asset,name,duration_days,channel_id FROM plans WHERE name=?1 LIMIT 1",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional().ok().flatten()
    }

    pub fn get_plan_by_id(
        &self,
        id: i64,
    ) -> Option<(i64, String, String, String, i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT seller_id,price,asset,name,duration_days,channel_id FROM plans WHERE id=?1",
            params![id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    // ---- Products ----
    #[allow(clippy::too_many_arguments)] // mirrors add_plan's flat form fields
    pub fn add_product(
        &self,
        seller_id: i64,
        name: &str,
        price: &str,
        asset: &str,
        kind: &str,
        delivery: Option<&str>,
        stock: Option<i64>,
    ) -> Result<i64, String> {
        if !matches!(kind, "digital_link" | "digital_file" | "physical") {
            return Err(format!(
                "invalid kind '{}' (expected digital_link, digital_file or physical)",
                kind
            ));
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Creation-time cross-table name-collision rejection: a name already
        // used by an active plan or product of the same seller cannot be
        // reused (names in other sellers' namespaces are fine).
        let plan_collision = conn
            .query_row(
                "SELECT 1 FROM plans WHERE seller_id=?1 AND name=?2 AND active=1 LIMIT 1",
                params![seller_id, name],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        if plan_collision {
            return Err(format!("name '{}' already used by an active plan", name));
        }
        let product_collision = conn
            .query_row(
                "SELECT 1 FROM products WHERE seller_id=?1 AND name=?2 AND active=1 LIMIT 1",
                params![seller_id, name],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        if product_collision {
            return Err(format!("name '{}' already used by an active product", name));
        }
        let now = Utc::now().to_rfc3339();
        // Upsert: a retired (active=0) product's (seller_id,name) slot may be
        // reused, mirroring add_plan's INSERT OR REPLACE behavior.
        conn.execute(
            "INSERT INTO products(seller_id,name,price,asset,kind,delivery,stock,active,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)
             ON CONFLICT(seller_id,name) DO UPDATE SET price=excluded.price, asset=excluded.asset, kind=excluded.kind, delivery=excluded.delivery, stock=excluded.stock, active=1",
            params![seller_id, name, price, asset, kind, delivery, stock, now],
        )
        .map_err(|e| e.to_string())?;
        let id: i64 = conn
            .query_row(
                "SELECT id FROM products WHERE seller_id=?1 AND name=?2",
                params![seller_id, name],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        Ok(id)
    }

    pub fn get_product(&self, seller_id: i64, name: &str) -> Option<ProductRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
        ).optional().ok().flatten()
    }

    /// Seller scope shows every row (incl. retired); unscoped listing is the
    /// public shop view, active products only.
    pub fn list_products(&self, seller: Option<i64>) -> Vec<ProductRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) = seller {
            ("SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products WHERE seller_id=?1 ORDER BY id".to_string(), vec![Box::new(sid)])
        } else {
            ("SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products WHERE active=1 ORDER BY id".to_string(), vec![])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn set_product_active(&self, seller_id: i64, name: &str, active: bool) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE products SET active=?3 WHERE seller_id=?1 AND name=?2",
            params![seller_id, name, if active { 1 } else { 0 }],
        );
    }

    /// Name lookup for /buy resolution, scoped to the (seller_id, name) plan
    /// namespace (2026-10-25 review HIGH): `Some(seller_id)` resolves one
    /// seller's namespace; `None` is the cross-seller active shop view (the
    /// caller decides which namespaces it may see — the global seller_id=0
    /// namespace is admin/owner-only, see handlers::commands).
    pub fn find_products_by_name(&self, seller_scope: Option<i64>, name: &str) -> Vec<ProductRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products WHERE seller_id=?1 AND name=?2 AND active=1 ORDER BY id".to_string(),
                vec![Box::new(sid), Box::new(name)],
            )
        } else {
            (
                "SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products WHERE name=?1 AND active=1 ORDER BY id".to_string(),
                vec![Box::new(name)],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Name lookup for /buy resolution, scoped to the (seller_id, name) plan
    /// namespace (2026-10-25 review HIGH): `Some(seller_id)` resolves one
    /// seller's namespace (seller_id 0 is the admin/owner global namespace);
    /// `None` is the cross-seller active view feeding the public shop.
    pub fn find_plans_by_name(&self, seller_scope: Option<i64>, name: &str) -> Vec<PlanRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT name,price,asset,duration_days,channel_id,active,seller_id FROM plans WHERE seller_id=?1 AND name=?2 AND active=1 ORDER BY id".to_string(),
                vec![Box::new(sid), Box::new(name)],
            )
        } else {
            (
                "SELECT name,price,asset,duration_days,channel_id,active,seller_id FROM plans WHERE name=?1 AND active=1 ORDER BY id".to_string(),
                vec![Box::new(name)],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get::<_, i64>(5)? != 0,
                        r.get(6)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn product_grant_exists(&self, invoice_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT 1 FROM product_grants WHERE invoice_id=?1 LIMIT 1",
            params![invoice_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    /// Atomically grant a product for a paid invoice: Ok(false) when the
    /// invoice was already granted (idempotent), Err("out of stock") when the
    /// conditional stock decrement matches no row. The exists-check,
    /// decrement and insert run in one lock scope (same serialization as
    /// claim_invoice_for_grant; the mutex is not reentrant, so the
    /// product_grant_exists helper cannot be reused here).
    pub fn grant_product(
        &self,
        seller_id: i64,
        user_id: i64,
        product_id: i64,
        invoice_id: &str,
    ) -> Result<bool, String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let exists = conn
            .query_row(
                "SELECT 1 FROM product_grants WHERE invoice_id=?1 LIMIT 1",
                params![invoice_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .ok()
            .flatten()
            .is_some();
        if exists {
            return Ok(false);
        }
        // NULL stock = unlimited; rows==0 means exhausted (or gone) -- no
        // grant may be created.
        let n = conn
            .execute(
                "UPDATE products SET stock=stock-1 WHERE id=?1 AND (stock IS NULL OR stock>0)",
                params![product_id],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("out of stock".to_string());
        }
        conn.execute(
            "INSERT INTO product_grants(seller_id,user_id,product_id,invoice_id,status,created_at) VALUES(?1,?2,?3,?4,'active',?5)",
            params![seller_id, user_id, product_id, invoice_id, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(true)
    }

    // ---- Orders (physical fulfillment, v1.3-lite) ----
    /// Create an order for a physical purchase. `order_id` is a fresh 8-char
    /// CSPRNG token; UNIQUE(invoice_id) keeps one order per purchase, so a
    /// duplicate insert for the same invoice surfaces as Err (callers log and
    /// continue — the grant itself is already durable).
    pub fn create_order(
        &self,
        seller_id: i64,
        product_id: i64,
        buyer: i64,
        invoice_id: &str,
    ) -> Result<String, String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let order_id = crate::util::gen_token(8);
        conn.execute(
            "INSERT INTO orders(order_id,seller_id,product_id,buyer_user_id,invoice_id,status,tracking,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,'pending',NULL,?6,?6)",
            params![order_id, seller_id, product_id, buyer, invoice_id, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(order_id)
    }

    /// Seller scope lists that seller's management view; None lists every
    /// order (admin/owner). Rows: (order_id, seller_id, buyer_user_id,
    /// status, tracking, created_at), newest first.
    pub fn list_orders(
        &self,
        seller: Option<i64>,
    ) -> Vec<(String, i64, i64, String, Option<String>, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) = seller {
            (
                "SELECT order_id,seller_id,buyer_user_id,status,tracking,created_at FROM orders WHERE seller_id=?1 ORDER BY id DESC".to_string(),
                vec![Box::new(sid)],
            )
        } else {
            (
                "SELECT order_id,seller_id,buyer_user_id,status,tracking,created_at FROM orders ORDER BY id DESC".to_string(),
                vec![],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Seller-scoped status transition. Returns false when the status is not
    /// one of pending|shipped|delivered|refunded|cancelled or the order is
    /// not this seller's.
    pub fn set_order_status(&self, seller_id: i64, order_id: &str, status: &str) -> bool {
        if !matches!(
            status,
            "pending" | "shipped" | "delivered" | "refunded" | "cancelled"
        ) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE orders SET status=?3, updated_at=?4 WHERE seller_id=?1 AND order_id=?2",
            params![seller_id, order_id, status, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn set_order_tracking(&self, seller_id: i64, order_id: &str, tracking: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE orders SET tracking=?3, updated_at=?4 WHERE seller_id=?1 AND order_id=?2",
            params![seller_id, order_id, tracking, now],
        );
    }

    // ---- Subscriptions ----
    #[allow(clippy::too_many_arguments)]
    pub fn create_subscription(
        &self,
        seller_id: i64,
        user_id: i64,
        plan_id: Option<i64>,
        channel_id: &str,
        days: i64,
        invoice_id: Option<&str>,
        code_id: Option<i64>,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now();
        let starts = now.to_rfc3339();
        // Checked arithmetic: a huge `days` must not panic (neither in
        // Duration::days nor in the addition). Clamp near the maximum
        // representable instant instead.
        let expires = chrono::Duration::try_days(days)
            .and_then(|d| now.checked_add_signed(d))
            .unwrap_or(DateTime::<Utc>::MAX_UTC - chrono::Duration::days(1))
            .to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO subscriptions(seller_id,user_id,plan_id,channel_id,status,starts_at,expires_at,invoice_id,code_id,created_at) VALUES(?1,?2,?3,?4,'active',?5,?6,?7,?8,?5)",
            params![seller_id, user_id, plan_id, channel_id, starts, expires, invoice_id, code_id],
        );
        conn.last_insert_rowid()
    }

    pub fn list_subscriptions(
        &self,
        seller_scope: Option<i64>,
        limit: i64,
    ) -> Vec<(i64, i64, i64, String, String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT id,seller_id,user_id,channel_id,status,expires_at FROM subscriptions WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2".to_string(), vec![Box::new(sid), Box::new(limit)])
        } else {
            ("SELECT id,seller_id,user_id,channel_id,status,expires_at FROM subscriptions ORDER BY id DESC LIMIT ?1".to_string(), vec![Box::new(limit)])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Bot-scoped variant of `list_subscriptions`: the rows of every seller in
    /// `seller_ids` (a bot's sellers, from `sellers_for_bot`), newest first and
    /// the same row shape/order as `list_subscriptions`. An empty slice
    /// short-circuits to `vec![]` — `IN ()` is a SQLite syntax error, so we
    /// never build it.
    pub fn list_subscriptions_for_sellers(
        &self,
        seller_ids: &[i64],
        limit: i64,
    ) -> Vec<(i64, i64, i64, String, String, String)> {
        if seller_ids.is_empty() {
            return vec![];
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let placeholders: String = (1..=seller_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<String>>()
            .join(",");
        let sql = format!(
            "SELECT id,seller_id,user_id,channel_id,status,expires_at FROM subscriptions WHERE seller_id IN ({placeholders}) ORDER BY id DESC LIMIT ?{}",
            seller_ids.len() + 1
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(seller_ids.len() + 1);
        for sid in seller_ids {
            params_vec.push(Box::new(*sid));
        }
        params_vec.push(Box::new(limit));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_subscriptions_filtered(
        &self,
        seller_scope: Option<i64>,
        status_opt: Option<&str>,
        limit: i64,
    ) -> Vec<(i64, i64, i64, String, String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = "SELECT id,seller_id,user_id,channel_id,status,expires_at FROM subscriptions"
            .to_string();
        let mut conds: Vec<String> = vec![];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if seller_scope.is_some() {
            conds.push("seller_id=?1".to_string());
        }
        if status_opt.is_some() {
            if seller_scope.is_some() {
                conds.push("status=?2".to_string());
            } else {
                conds.push("status=?1".to_string());
            }
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        if let Some(sid) = seller_scope {
            params_vec.push(Box::new(sid));
        }
        if let Some(st) = status_opt {
            params_vec.push(Box::new(st.to_string()));
        }
        let limit_idx = params_vec.len() + 1;
        sql.push_str(&format!(" ORDER BY id DESC LIMIT ?{}", limit_idx));
        params_vec.push(Box::new(limit));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Paged + searchable variant of `list_subscriptions_filtered`. `search`
    /// matches user_id (as text) or channel_id (substring). `offset` is a row
    /// offset (0-based) applied after ORDER BY id DESC.
    pub fn list_subscriptions_filtered_paged(
        &self,
        seller_scope: Option<i64>,
        status_opt: Option<&str>,
        search: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Vec<(i64, i64, i64, String, String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = "SELECT id,seller_id,user_id,channel_id,status,expires_at FROM subscriptions"
            .to_string();
        let mut conds: Vec<String> = vec![];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if let Some(sid) = seller_scope {
            conds.push("seller_id=?".to_string());
            params_vec.push(Box::new(sid));
        }
        if let Some(st) = status_opt
            && st != "all"
        {
            conds.push("status=?".to_string());
            params_vec.push(Box::new(st.to_string()));
        }
        if let Some(q) = search
            && !q.trim().is_empty()
        {
            let q = q.trim();
            conds.push("(CAST(user_id AS TEXT) LIKE ? OR channel_id LIKE ?)".to_string());
            params_vec.push(Box::new(format!("%{}%", q)));
            params_vec.push(Box::new(format!("%{}%", q)));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        sql.push_str(" ORDER BY id DESC LIMIT ? OFFSET ?");
        params_vec.push(Box::new(limit));
        params_vec.push(Box::new(offset));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Count matching `list_subscriptions_filtered_paged` (for pagination).
    pub fn count_subscriptions_filtered(
        &self,
        seller_scope: Option<i64>,
        status_opt: Option<&str>,
        search: Option<&str>,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = "SELECT COUNT(*) FROM subscriptions".to_string();
        let mut conds: Vec<String> = vec![];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![];
        if let Some(sid) = seller_scope {
            conds.push("seller_id=?".to_string());
            params_vec.push(Box::new(sid));
        }
        if let Some(st) = status_opt
            && st != "all"
        {
            conds.push("status=?".to_string());
            params_vec.push(Box::new(st.to_string()));
        }
        if let Some(q) = search
            && !q.trim().is_empty()
        {
            let q = q.trim();
            conds.push("(CAST(user_id AS TEXT) LIKE ? OR channel_id LIKE ?)".to_string());
            params_vec.push(Box::new(format!("%{}%", q)));
            params_vec.push(Box::new(format!("%{}%", q)));
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let res = stmt.query_row(
            rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
            |r| r.get::<_, i64>(0),
        );
        res.unwrap_or(0)
    }

    pub fn expired_subscriptions(&self) -> Vec<(i64, i64, i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let mut stmt = match conn.prepare("SELECT id,seller_id,user_id,channel_id FROM subscriptions WHERE status='active' AND expires_at < ?1") { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![now], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Seller-scoped counterpart to `expired_subscriptions`: atomically mark
    /// only this seller's active-but-expired rows and return how many changed.
    /// The unconditional `seller_id` predicate is the isolation boundary; the
    /// global sweep (`expired_subscriptions` + `mark_subscription_expired`)
    /// stays available for the owner path and the periodic sweeper.
    pub fn expire_subscriptions_for_seller(&self, seller_id: i64) -> usize {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE subscriptions SET status='expired' WHERE seller_id=?1 AND status='active' AND expires_at < ?2",
            params![seller_id, now],
        )
        .unwrap_or(0)
    }

    pub fn mark_subscription_expired(&self, id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE subscriptions SET status='expired' WHERE id=?1",
            params![id],
        );
    }
    pub fn subscriptions_for_invoice(&self, invoice_id: &str) -> Vec<(i64, i64, i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,user_id,channel_id FROM subscriptions WHERE invoice_id=?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![invoice_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn set_subscription_status(&self, sub_id: i64, status: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE subscriptions SET status=?1 WHERE id=?2",
            params![status, sub_id],
        );
    }

    /// Subscription bound to an invoice: (id, channel_id). Safe with the
    /// one-subscription-per-invoice unique index.
    pub fn subscription_by_invoice(&self, invoice_id: &str) -> Option<(i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id, channel_id FROM subscriptions WHERE invoice_id=?1 LIMIT 1",
            params![invoice_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn user_has_active_sub(&self, seller_id: i64, user_id: i64, channel_id: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.query_row("SELECT 1 FROM subscriptions WHERE seller_id=?1 AND user_id=?2 AND channel_id=?3 AND status='active' AND expires_at > ?4", params![seller_id, user_id, channel_id, now], |r| r.get::<_, i64>(0))
            .optional().ok().flatten().is_some()
    }

    // ---- Codes ----
    pub fn create_code(
        &self,
        seller_id: i64,
        plan_id: Option<i64>,
        value_days: Option<i64>,
        max_uses: i64,
        expires_at: Option<&str>,
        length: usize,
    ) -> String {
        let length = if length == 0 { 12 } else { length.clamp(1, 32) };
        let code = crate::util::gen_token(length);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO activation_codes(seller_id,code,plan_id,value_days,max_uses,uses,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,0,?6,?7)",
            params![seller_id, code, plan_id, value_days, max_uses, expires_at, now],
        );
        code
    }

    pub fn get_code(&self, code: &str) -> Option<CodeRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,plan_id,value_days,max_uses,uses,expires_at FROM activation_codes WHERE code=?1",
            params![code],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional().ok().flatten()
    }

    pub fn redeem_code(
        &self,
        code: &str,
        user_id: i64,
    ) -> Result<(i64, Option<i64>, Option<i64>), String> {
        let (id, seller_id, plan_id, value_days, max_uses, uses, expires_at) = self
            .get_code(code)
            .ok_or_else(|| "invalid code".to_string())?;
        // reject revoked
        let revoked: i64 = {
            let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            conn.query_row(
                "SELECT revoked FROM activation_codes WHERE code=?1",
                params![code],
                |r| r.get(0),
            )
            .unwrap_or(0)
        };
        if revoked != 0 {
            return Err("code revoked".to_string());
        }
        if uses >= max_uses {
            return Err("code already used".to_string());
        }
        if let Some(exp) = &expires_at
            && Utc::now().to_rfc3339() > *exp
        {
            return Err("code expired".to_string());
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE activation_codes SET uses = uses + 1 WHERE id = ?1",
            params![id],
        );
        let _ = conn.execute(
            "INSERT INTO code_redemptions(code_id,user_id,redeemed_at) VALUES(?1,?2,?3)",
            params![id, user_id, now],
        );
        Ok((seller_id, plan_id, value_days))
    }

    pub fn revoke_code(&self, code: &str, scope: Option<i64>) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE activation_codes SET revoked=1 WHERE code=?1 AND (?2 IS NULL OR seller_id=?2)",
            params![code, scope],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn reclaim_code(
        &self,
        old_code: &str,
        scope: Option<i64>,
        length: usize,
    ) -> Option<String> {
        // scope check + fetch old
        let row = self.get_code(old_code)?;
        let (_id, seller_id, plan_id, value_days, max_uses, _uses, expires_at) = row;
        if let Some(s) = scope
            && seller_id != s
        {
            return None;
        }
        // revoke old
        let _ = self.revoke_code(old_code, scope);
        // create new copying fields
        let new_code = self.create_code(
            seller_id,
            plan_id,
            value_days,
            max_uses,
            expires_at.as_deref(),
            length,
        );
        Some(new_code)
    }

    pub fn list_codes(&self, seller_scope: Option<i64>, limit: i64) -> Vec<CodeListRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT code,plan_id,value_days,uses,max_uses,seller_id,revoked FROM activation_codes WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2".to_string(), vec![Box::new(sid), Box::new(limit)])
        } else {
            ("SELECT code,plan_id,value_days,uses,max_uses,seller_id,revoked FROM activation_codes ORDER BY id DESC LIMIT ?1".to_string(), vec![Box::new(limit)])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get::<_, i64>(6)? != 0,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Bot-scoped variant of `list_codes`: the activation codes of every seller
    /// in `seller_ids` (a bot's sellers, from `sellers_for_bot`), newest first,
    /// with the same columns/order as `list_codes`. An empty slice
    /// short-circuits to `vec![]` — `IN ()` is a SQLite syntax error, so we
    /// never build it.
    pub fn list_codes_for_sellers(&self, seller_ids: &[i64], limit: i64) -> Vec<CodeListRow> {
        if seller_ids.is_empty() {
            return vec![];
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let placeholders: String = (1..=seller_ids.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<String>>()
            .join(",");
        let sql = format!(
            "SELECT code,plan_id,value_days,uses,max_uses,seller_id,revoked FROM activation_codes WHERE seller_id IN ({placeholders}) ORDER BY id DESC LIMIT ?{}",
            seller_ids.len() + 1
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> =
            Vec::with_capacity(seller_ids.len() + 1);
        for sid in seller_ids {
            params_vec.push(Box::new(*sid));
        }
        params_vec.push(Box::new(limit));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get::<_, i64>(6)? != 0,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Redemption history for one activation code, newest first. The code
    /// string is resolved to its row via a join (no separate lookup needed).
    /// `seller_scope` mirrors the other scoped list methods: `Some(id)`
    /// (seller role) returns only redemptions of that seller's own code,
    /// `None` (admin/owner) all of them.
    pub fn list_code_redemptions(
        &self,
        code: &str,
        seller_scope: Option<i64>,
        limit: i64,
    ) -> Vec<CodeRedemptionRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT cr.id,cr.user_id,cr.redeemed_at FROM code_redemptions cr JOIN activation_codes ac ON ac.id=cr.code_id WHERE ac.code=?1 AND ac.seller_id=?2 ORDER BY cr.id DESC LIMIT ?3"
                    .to_string(),
                vec![Box::new(code), Box::new(sid), Box::new(limit)],
            )
        } else {
            (
                "SELECT cr.id,cr.user_id,cr.redeemed_at FROM code_redemptions cr JOIN activation_codes ac ON ac.id=cr.code_id WHERE ac.code=?1 ORDER BY cr.id DESC LIMIT ?2"
                    .to_string(),
                vec![Box::new(code), Box::new(limit)],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- Users ----
    pub fn upsert_user(&self, user_id: i64, username: Option<&str>) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO users(user_id,username,first_seen,last_seen) VALUES(?1,?2,?3,?3)
             ON CONFLICT(user_id) DO UPDATE SET username=excluded.username, last_seen=excluded.last_seen",
            params![user_id, username, now],
        );
    }

    pub fn list_users(&self) -> Vec<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT user_id FROM users") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_users_for_seller(&self, seller_id: i64) -> Vec<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt =
            match conn.prepare("SELECT DISTINCT user_id FROM subscriptions WHERE seller_id=?1") {
                Ok(s) => s,
                Err(_) => return vec![],
            };
        let rows = stmt
            .query_map(params![seller_id], |r| r.get::<_, i64>(0))
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- AI ----
    #[allow(clippy::too_many_arguments)]
    pub fn record_ai_usage(
        &self,
        seller_id: i64,
        user_id: Option<i64>,
        provider: &str,
        model: &str,
        template: Option<&str>,
        prompt_tokens: i64,
        completion_tokens: i64,
        total_tokens: i64,
        estimated_cost: f64,
        status: &str,
        error_message: Option<&str>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO ai_usage(seller_id,user_id,provider,model,template,prompt_tokens,completion_tokens,total_tokens,estimated_cost,status,error_message,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![seller_id, user_id, provider, model, template, prompt_tokens, completion_tokens, total_tokens, estimated_cost, status, error_message, now],
        );
    }

    pub fn ai_usage_today_tokens(&self, seller_id: i64) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let today = Utc::now().format("%Y-%m-%d").to_string();
        conn.query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM ai_usage WHERE seller_id=?1 AND created_at LIKE ?2",
            params![seller_id, format!("{}%", today)],
            |r| r.get(0),
        ).unwrap_or(0)
    }

    pub fn ai_usage_summary(&self, seller_id: i64, days: i64) -> Vec<(String, i64, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT substr(created_at,1,10) as d, COALESCE(SUM(total_tokens),0), COUNT(*) FROM ai_usage WHERE seller_id=?1 GROUP BY d ORDER BY d DESC LIMIT ?2",
        ) { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![seller_id, days], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn ai_usage_today_tokens_global(&self) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let today = Utc::now().format("%Y-%m-%d").to_string();
        conn.query_row(
            "SELECT COALESCE(SUM(total_tokens),0) FROM ai_usage WHERE created_at LIKE ?1",
            params![format!("{}%", today)],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn ai_usage_summary_global(&self, days: i64) -> Vec<(String, i64, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT substr(created_at,1,10) as d, COALESCE(SUM(total_tokens),0), COUNT(*) FROM ai_usage GROUP BY d ORDER BY d DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![days], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn ai_usage_cost_total_global(&self, days: i64) -> f64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cutoff = (Utc::now() - chrono::Duration::days(days))
            .format("%Y-%m-%d")
            .to_string();
        conn.query_row(
            "SELECT COALESCE(SUM(estimated_cost),0.0) FROM ai_usage WHERE created_at >= ?1",
            params![cutoff],
            |r| r.get(0),
        )
        .unwrap_or(0.0)
    }

    pub fn ai_usage_recent_errors_global(
        &self,
        limit: i64,
    ) -> Vec<(String, String, String, Option<String>, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT created_at, provider, model, error_message, status FROM ai_usage WHERE status != 'ok' OR error_message IS NOT NULL ORDER BY created_at DESC, id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn ai_usage_recent_errors(
        &self,
        seller_id: i64,
        limit: i64,
    ) -> Vec<(String, String, String, Option<String>, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT created_at, provider, model, error_message, status FROM ai_usage WHERE seller_id=?1 AND (status != 'ok' OR error_message IS NOT NULL) ORDER BY created_at DESC, id DESC LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![seller_id, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// List AI usage rows for export (newest first). `seller_scope` None = all sellers
    /// (owner view). Returns (id, seller_id, user_id, provider, model, template,
    /// prompt_tokens, completion_tokens, total_tokens, estimated_cost, status,
    /// error_message, created_at).
    pub fn list_ai_usage(
        &self,
        seller_scope: Option<i64>,
        limit: i64,
    ) -> Vec<(
        i64,
        i64,
        Option<i64>,
        String,
        String,
        Option<String>,
        i64,
        i64,
        i64,
        f64,
        String,
        Option<String>,
        String,
    )> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            (
                "SELECT id,seller_id,user_id,provider,model,template,prompt_tokens,completion_tokens,total_tokens,estimated_cost,status,error_message,created_at FROM ai_usage WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2"
                    .to_string(),
                vec![Box::new(sid), Box::new(limit)],
            )
        } else {
            (
                "SELECT id,seller_id,user_id,provider,model,template,prompt_tokens,completion_tokens,total_tokens,estimated_cost,status,error_message,created_at FROM ai_usage ORDER BY id DESC LIMIT ?1"
                    .to_string(),
                vec![Box::new(limit)],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                        r.get(10)?,
                        r.get(11)?,
                        r.get(12)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_seller_ai_settings(&self, seller_id: i64) -> Option<SellerAiSettings> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT ai_provider, ai_model, daily_token_budget, rpm_limit FROM sellers WHERE user_id=?1",
            params![seller_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional().ok().flatten()
    }

    pub fn set_seller_ai_settings(
        &self,
        seller_id: i64,
        provider: Option<&str>,
        model: Option<&str>,
        budget: Option<i64>,
        rpm: Option<i64>,
    ) {
        self.upsert_seller(seller_id, None, None);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE sellers SET ai_provider=COALESCE(?1,ai_provider), ai_model=COALESCE(?2,ai_model), daily_token_budget=COALESCE(?3,daily_token_budget), rpm_limit=COALESCE(?4,rpm_limit) WHERE user_id=?5",
            params![provider, model, budget, rpm, seller_id],
        );
    }

    pub fn upsert_ai_template(
        &self,
        seller_id: i64,
        name: &str,
        description: Option<&str>,
        system_prompt: Option<&str>,
        user_prompt: &str,
        variables: Option<&str>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO ai_templates(seller_id,name,description,system_prompt,user_prompt,variables,active,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,1,?7,?7)
             ON CONFLICT(seller_id,name) DO UPDATE SET description=excluded.description, system_prompt=excluded.system_prompt, user_prompt=excluded.user_prompt, variables=excluded.variables, updated_at=excluded.updated_at",
            params![seller_id, name, description, system_prompt, user_prompt, variables, now],
        );
    }

    pub fn get_ai_template(&self, seller_id: i64, name: &str) -> Option<AiTemplateRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT name, description, system_prompt, user_prompt, variables FROM ai_templates WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional().ok().flatten()
    }

    pub fn list_ai_templates(&self, seller_id: i64) -> Vec<(String, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT name, description FROM ai_templates WHERE seller_id=?1 AND active=1 ORDER BY name") { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![seller_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn delete_ai_template(&self, seller_id: i64, name: &str) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "DELETE FROM ai_templates WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
        );
    }

    // ---- Web accounts ----
    // bcrypt at DEFAULT_COST is deliberately CPU-heavy (~100ms per call).
    // Every hash/verify below runs on tokio's blocking pool (spawn_blocking)
    // so an async worker is never stalled by password math — login storms
    // would otherwise freeze the whole runtime (2026-10-25 review). Callers
    // are async handlers/tasks; the methods are async for exactly that
    // reason. Behavior is otherwise identical to the former sync versions.
    pub async fn create_web_account(
        &self,
        username: &str,
        password: &str,
        role: &str,
        seller_id: Option<i64>,
    ) -> Result<(), String> {
        let password = password.to_string();
        let hash =
            tokio::task::spawn_blocking(move || bcrypt::hash(&password, bcrypt::DEFAULT_COST))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO web_accounts(username,password_hash,salt,role,seller_id,created_at) VALUES(?1,?2,'',?3,?4,?5)",
            params![username, hash, role, seller_id, now],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_web_account_by_username(
        &self,
        username: &str,
    ) -> Option<(i64, String, String, String, Option<i64>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id, username, password_hash, role, seller_id FROM web_accounts WHERE username=?1",
            params![username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        ).optional().ok().flatten()
    }

    pub async fn verify_web_password(
        &self,
        username: &str,
        password: &str,
    ) -> Option<(i64, String, Option<i64>)> {
        let (id, _u, hash, role, seller_id) = self.get_web_account_by_username(username)?;
        // Owned copy: spawn_blocking needs 'static, so the &str param cannot
        // be captured by reference.
        let password = password.to_string();
        let ok =
            tokio::task::spawn_blocking(move || bcrypt::verify(&password, &hash).unwrap_or(false))
                .await
                .unwrap_or(false);
        if ok {
            Some((id, role, seller_id))
        } else {
            None
        }
    }

    pub async fn update_web_password(&self, username: &str, password: &str) -> Result<(), String> {
        let password = password.to_string();
        let hash =
            tokio::task::spawn_blocking(move || bcrypt::hash(&password, bcrypt::DEFAULT_COST))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let n = conn
            .execute(
                "UPDATE web_accounts SET password_hash=?1 WHERE username=?2",
                params![hash, username],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("account not found".to_string());
        }
        Ok(())
    }

    pub fn list_web_accounts(&self) -> Vec<(String, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT username, role FROM web_accounts ORDER BY id") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_web_accounts_full(&self) -> Vec<(i64, String, String, Option<i64>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn
            .prepare("SELECT id, username, role, seller_id FROM web_accounts ORDER BY id")
        {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_web_account_by_id(
        &self,
        id: i64,
    ) -> Option<(i64, String, String, String, Option<i64>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id, username, password_hash, role, seller_id FROM web_accounts WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn delete_web_account(&self, id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "DELETE FROM web_recovery_codes WHERE account_id=?1",
            params![id],
        );
        let _ = conn.execute(
            "DELETE FROM web_pending_mfa WHERE account_id=?1",
            params![id],
        );
        let _ = conn.execute("DELETE FROM web_accounts WHERE id=?1", params![id]);
        let _ = conn.execute("DELETE FROM web_sessions WHERE account_id=?1", params![id]);
    }

    // ---- MFA / TOTP helpers (Phase 4) ----
    pub fn set_totp_secret<S>(&self, account_id: i64, secret: Option<S>) -> Result<(), String>
    where
        S: AsRef<str>,
    {
        let secret = secret.map(|s| s.as_ref().to_string());
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE web_accounts SET totp_secret=?1 WHERE id=?2",
            params![secret, account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_totp_state(&self, account_id: i64) -> (Option<String>, bool, Option<i64>) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT totp_secret, totp_enabled, totp_last_used FROM web_accounts WHERE id=?1",
            params![account_id],
            |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, i64>(1).unwrap_or(0) != 0,
                    r.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or((None, false, None))
    }

    pub fn enable_totp<S: AsRef<str>>(&self, account_id: i64, secret: S) -> Result<(), String> {
        let secret = secret.as_ref().to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE web_accounts SET totp_secret=?1, totp_enabled=1 WHERE id=?2",
            params![secret, account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn disable_totp(&self, account_id: i64) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE web_accounts SET totp_secret=NULL, totp_enabled=0, totp_last_used=NULL WHERE id=?1",
            params![account_id],
        )
        .map_err(|e| e.to_string())?;
        let _ = conn.execute(
            "DELETE FROM web_recovery_codes WHERE account_id=?1",
            params![account_id],
        );
        let _ = conn.execute(
            "DELETE FROM web_pending_mfa WHERE account_id=?1",
            params![account_id],
        );
        Ok(())
    }

    pub fn update_totp_last_used<T>(&self, account_id: i64, counter: T) -> Result<(), String>
    where
        T: TryInto<i64>,
        T::Error: std::fmt::Debug,
    {
        let counter: i64 = counter.try_into().unwrap_or(0);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE web_accounts SET totp_last_used=?1 WHERE id=?2",
            params![counter, account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn create_pending_mfa<TH, P, TS, E>(
        &self,
        token_hash: TH,
        account_id: i64,
        purpose: P,
        totp_secret: Option<TS>,
        expires_at: E,
    ) -> Result<(), String>
    where
        TH: AsRef<str>,
        P: AsRef<str>,
        TS: AsRef<str>,
        E: ToString,
    {
        let token_hash = token_hash.as_ref().to_string();
        let purpose = purpose.as_ref().to_string();
        let totp_secret = totp_secret.map(|s| s.as_ref().to_string());
        let expires_at = expires_at.to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR REPLACE INTO web_pending_mfa(token_hash,account_id,purpose,totp_secret,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![token_hash, account_id, purpose, totp_secret, expires_at, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn get_pending_mfa<S: AsRef<str>>(
        &self,
        token: S,
    ) -> Option<(i64, String, Option<String>, String)> {
        let token = token.as_ref().to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT account_id, purpose, totp_secret, expires_at FROM web_pending_mfa WHERE token_hash=?1",
            params![token],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn delete_pending_mfa<S: AsRef<str>>(&self, token: S) -> Result<(), String> {
        let token = token.as_ref().to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM web_pending_mfa WHERE token_hash=?1",
            params![token],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_pending_mfa_for_account(&self, account_id: i64) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM web_pending_mfa WHERE account_id=?1",
            params![account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Mint a one-time console setup link token (D7, ARCHITECTURE-V1.4 §4):
    /// `gen_token(48)` stored SHA-256-hashed in web_pending_mfa under
    /// purpose 'setup_link' with a 15-minute TTL (the pending-MFA expiry
    /// writing style). Returns the RAW token exactly once — the caller builds
    /// the URL; the database never sees the raw value.
    pub fn create_setup_link(&self, account_id: i64) -> String {
        let raw = crate::util::gen_token(48);
        let hash = sha256_hex(&raw);
        let expires = (Utc::now() + chrono::Duration::minutes(SETUP_LINK_TTL_MINUTES)).to_rfc3339();
        let _ = self.create_pending_mfa(
            hash,
            account_id,
            SETUP_LINK_PURPOSE,
            None::<String>,
            expires,
        );
        raw
    }

    /// Atomic single-use claim of a pending token (D7): deletes the row and
    /// returns its account in ONE statement (SQLite RETURNING), so exactly one
    /// caller can win — replays, unknown and expired tokens are all None with
    /// no oracle. `purpose` keeps the MFA verify flow and setup links in
    /// separate lanes; the raw token is hashed here, matching
    /// create_pending_mfa's storage recipe.
    pub fn consume_pending_token(&self, raw_token: &str, purpose: &str) -> Option<i64> {
        let hash = sha256_hex(raw_token);
        let now = Utc::now().to_rfc3339();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "DELETE FROM web_pending_mfa WHERE token_hash=?1 AND purpose=?2 AND expires_at > ?3 RETURNING account_id",
            params![hash, purpose, now],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn create_recovery_codes<I, S>(&self, account_id: i64, hashed: I) -> Result<(), String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        for h in hashed {
            conn.execute(
                "INSERT INTO web_recovery_codes(account_id,code_hash,used,created_at) VALUES(?1,?2,0,?3)",
                params![account_id, h.as_ref().to_string(), now],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn verify_recovery_code<S: AsRef<str>>(&self, account_id: i64, code: S) -> bool {
        let code = code.as_ref().to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // fetch unused hashes
        let mut stmt = match conn
            .prepare("SELECT id, code_hash FROM web_recovery_codes WHERE account_id=?1 AND used=0")
        {
            Ok(s) => s,
            Err(_) => return false,
        };
        let rows: Vec<(i64, String)> = stmt
            .query_map(params![account_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()
            .map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default();
        for (id, hash) in rows {
            let ok = bcrypt::verify(&code, &hash).unwrap_or(false);
            if ok {
                let _ = conn.execute(
                    "UPDATE web_recovery_codes SET used=1 WHERE id=?1",
                    params![id],
                );
                return true;
            }
        }
        false
    }

    pub fn delete_recovery_codes(&self, account_id: i64) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM web_recovery_codes WHERE account_id=?1",
            params![account_id],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn list_unused_recovery_codes(&self, account_id: i64) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM web_recovery_codes WHERE account_id=?1 AND used=0",
            params![account_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    /// Alias matching task wording.
    pub fn list_unused(&self, account_id: i64) -> i64 {
        self.list_unused_recovery_codes(account_id)
    }

    pub fn list_unused_count(&self, account_id: i64) -> i64 {
        self.list_unused_recovery_codes(account_id)
    }

    /// CLI recovery: delete all owner accounts and their sessions, mint a
    /// fresh claim code. All writes run in a single BEGIN IMMEDIATE
    /// transaction. Returns the new claim code.
    pub fn delete_owners_and_mint_claim_code(&self) -> String {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute_batch("BEGIN IMMEDIATE");
        let _ = conn.execute(
            "DELETE FROM web_sessions WHERE account_id IN (SELECT id FROM web_accounts WHERE role = 'owner')",
            [],
        );
        let _ = conn.execute("DELETE FROM web_recovery_codes WHERE account_id IN (SELECT id FROM web_accounts WHERE role = 'owner')", []);
        let _ = conn.execute("DELETE FROM web_pending_mfa WHERE account_id IN (SELECT id FROM web_accounts WHERE role = 'owner')", []);
        let _ = conn.execute("DELETE FROM web_accounts WHERE role = 'owner'", []);
        let code = crate::util::gen_token(12);
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO config(key,value,updated_at) VALUES('claim_code',?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value, updated_at=excluded.updated_at",
            params![code, now],
        );
        let _ = conn.execute_batch("COMMIT");
        code
    }

    /// CLI recovery: create the first owner account if none exists, otherwise
    /// reset an existing owner account's password (and revoke its sessions).
    /// All DB writes run in a single BEGIN IMMEDIATE transaction. MFA state
    /// is cleared where applicable (no-op until MFA columns exist).
    pub async fn reset_owner_account(&self, username: &str, password: &str) -> Result<(), String> {
        let password = password.to_string();
        let hash =
            tokio::task::spawn_blocking(move || bcrypt::hash(&password, bcrypt::DEFAULT_COST))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;

        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute_batch("BEGIN IMMEDIATE");

        let run = (|| -> Result<(), String> {
            let owner_exists: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM web_accounts WHERE role = 'owner')",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|v| v != 0)
                .unwrap_or(false);

            if !owner_exists {
                // Create (or repair) the owner account for this username.
                let existing_id: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM web_accounts WHERE username = ?1",
                        params![username],
                        |r| r.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten();
                if let Some(id) = existing_id {
                    conn.execute(
                        "UPDATE web_accounts SET password_hash = ?1, role = 'owner', seller_id = NULL, totp_secret = NULL, totp_enabled = 0, totp_last_used = NULL WHERE id = ?2",
                        params![hash, id],
                    )
                    .map_err(|e| e.to_string())?;
                    conn.execute(
                        "DELETE FROM web_sessions WHERE account_id = ?1",
                        params![id],
                    )
                    .map_err(|e| e.to_string())?;
                } else {
                    let now = Utc::now().to_rfc3339();
                    conn.execute(
                        "INSERT INTO web_accounts(username,password_hash,salt,role,seller_id,totp_secret,totp_enabled,totp_last_used,created_at) VALUES(?1,?2,'','owner',NULL,NULL,0,NULL,?3)",
                        params![username, hash, now],
                    )
                    .map_err(|e| e.to_string())?;
                }
            } else {
                // Reset password on the matching owner account, or all owners if
                // the requested username is not itself an owner account.
                let mut ids: Vec<i64> = Vec::new();
                if let Ok(id) = conn.query_row(
                    "SELECT id FROM web_accounts WHERE username = ?1 AND role = 'owner'",
                    params![username],
                    |r| r.get::<_, i64>(0),
                ) {
                    ids.push(id);
                }
                if ids.is_empty() {
                    let mut stmt = conn
                        .prepare("SELECT id FROM web_accounts WHERE role = 'owner'")
                        .map_err(|e| e.to_string())?;
                    let rows = stmt
                        .query_map([], |r| r.get::<_, i64>(0))
                        .map_err(|e| e.to_string())?;
                    for id in rows.flatten() {
                        ids.push(id);
                    }
                }
                for id in ids {
                    conn.execute(
                        "UPDATE web_accounts SET password_hash = ?1, totp_secret = NULL, totp_enabled = 0, totp_last_used = NULL WHERE id = ?2",
                        params![hash, id],
                    )
                    .map_err(|e| e.to_string())?;
                    conn.execute(
                        "DELETE FROM web_sessions WHERE account_id = ?1",
                        params![id],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        })();

        if run.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else {
            let _ = conn.execute_batch("COMMIT");
        }
        run
    }

    pub async fn update_web_password_by_id(&self, id: i64, password: &str) -> Result<(), String> {
        let password = password.to_string();
        let hash =
            tokio::task::spawn_blocking(move || bcrypt::hash(&password, bcrypt::DEFAULT_COST))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let n = conn
            .execute(
                "UPDATE web_accounts SET password_hash=?1 WHERE id=?2",
                params![hash, id],
            )
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("account not found".to_string());
        }
        Ok(())
    }

    // ---- Web sessions ----
    pub fn create_web_session(
        &self,
        account_id: i64,
        role: &str,
        seller_id: Option<i64>,
        days: i64,
    ) -> String {
        let token = crate::util::gen_token(48);
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(token.as_bytes());
            let bytes = h.finalize();
            let mut s = String::with_capacity(64);
            for b in bytes.iter() {
                s.push_str(&format!("{:02x}", b));
            }
            s
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now();
        let expires = (now + chrono::Duration::days(days)).to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO web_sessions(token_hash,account_id,role,seller_id,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![hash, account_id, role, seller_id, expires, now.to_rfc3339()],
        );
        token
    }

    pub fn get_web_session(&self, token: &str) -> Option<(i64, String, Option<i64>, Option<i64>)> {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(token.as_bytes());
        let bytes = h.finalize();
        let mut hash = String::with_capacity(64);
        for b in bytes.iter() {
            hash.push_str(&format!("{:02x}", b));
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.query_row(
            "SELECT account_id, role, seller_id, selected_bot_id FROM web_sessions WHERE token_hash=?1 AND expires_at > ?2",
            params![hash, now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional().ok().flatten()
    }

    /// Persist the selected bot for a web session (by token hash). Returns
    /// whether a row was updated.
    pub fn set_web_session_selected_bot(&self, token_hash: &str, bot_id: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE web_sessions SET selected_bot_id = ?1 WHERE token_hash = ?2",
            params![bot_id, token_hash],
        ) == Ok(1)
    }

    /// Read the selected bot id for a web session (by token hash). Returns
    /// None when the session is missing/expired or has no selection.
    pub fn get_web_session_selected_bot(&self, token_hash: &str) -> Option<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.query_row(
            "SELECT selected_bot_id FROM web_sessions WHERE token_hash = ?1 AND expires_at > ?2",
            params![token_hash, now],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()
        .ok()
        .flatten()
        .flatten()
    }

    pub fn delete_web_session(&self, token: &str) {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(token.as_bytes());
        let bytes = h.finalize();
        let mut hash = String::with_capacity(64);
        for b in bytes.iter() {
            hash.push_str(&format!("{:02x}", b));
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "DELETE FROM web_sessions WHERE token_hash=?1",
            params![hash],
        );
    }

    // ---- Refunds ----

    /// Build a fail-closed rusqlite error carrying an operator-readable reason.
    /// rusqlite 0.41 removed `Error::Msg`, so use a SQLite constraint failure
    /// with a message; `Db::new` propagates it verbatim to the operator.
    fn sqlite_fail(msg: impl Into<String>) -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some(msg.into()),
        )
    }

    /// Parse a stored amount as base-10 decimal integer minor units at
    /// scale 8 (spec §1.2). No `f64`, no `CAST(... AS REAL)`. Negative,
    /// non-finite, over-precise or overflowing values are unquantizable and
    /// must fail closed until an owner resolves them.
    fn minor_units(value: &str) -> Option<i128> {
        let text = value.trim();
        if text.is_empty() || text.starts_with('-') || text.starts_with('+') {
            return None;
        }
        let mut pieces = text.split('.');
        let whole = pieces.next()?;
        let frac = pieces.next().unwrap_or("");
        if pieces.next().is_some()
            || whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
            || frac.len() > MONEY_MINOR_SCALE as usize
        {
            return None;
        }
        let whole_units: i128 = whole.parse().ok()?;
        // `"".parse::<i128>()` fails; an empty fractional part is zero.
        let mut frac_units: i128 = if frac.is_empty() {
            0
        } else {
            frac.parse().ok()?
        };
        for _ in frac.len()..MONEY_MINOR_SCALE as usize {
            frac_units = frac_units.checked_mul(10)?;
        }
        whole_units
            .checked_mul(10_i128.pow(MONEY_MINOR_SCALE))?
            .checked_add(frac_units)
    }

    /// Legacy-compatible wrapper (api.rs / web finance still call this with
    /// the original six arguments). It now runs the full §1.2 safety block,
    /// synthesizing a unique sentinel idem key and the documented
    /// `created_by = -1` / `created_role = 'unknown'` actor. The server switches
    /// those call sites to `create_refund_guarded` so the real actor is
    /// recorded. Returns 0 on any refusal, matching the old "insert failed"
    /// shape; never returns an id for a refund that was not created.
    pub fn create_refund(
        &self,
        seller_id: i64,
        invoice_id: Option<&str>,
        user_id: Option<i64>,
        amount: &str,
        asset: &str,
        reason: Option<&str>,
    ) -> i64 {
        let seq = REFUND_IDEM_SEQ.fetch_add(1, Ordering::Relaxed);
        let idem_key = format!(
            "auto:{}:{}:{}",
            Utc::now().timestamp_micros(),
            std::process::id(),
            seq
        );
        match self.create_refund_guarded(RefundInput {
            seller_id,
            invoice_id,
            user_id,
            amount,
            asset,
            reason,
            idem_key: &idem_key,
            created_by: -1,
            created_role: "unknown",
        }) {
            Ok(created) => created.id,
            Err(_) => 0,
        }
    }

    /// §1.2 creation algorithm in ONE `BEGIN IMMEDIATE`: WAL + busy_timeout
    /// (`Db::new`) makes the immediate transaction the serialization point,
    /// so two concurrent creators cannot both pass the cumulative cap.
    /// An idem-key replay returns the existing row (no second money event);
    /// a distinct key while another refund is in flight is refused.
    pub fn create_refund_guarded(
        &self,
        input: RefundInput<'_>,
    ) -> Result<RefundCreation, RefundCreateError> {
        use RefundCreateError::*;
        let mut conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| Database(e.to_string()))?;
        let now = Utc::now().to_rfc3339();

        // Idempotent replay first: same key for the same invoice is the SAME
        // money movement, whatever state that row has reached.
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM refunds WHERE invoice_id IS ?1 AND idem_key=?2 ORDER BY id LIMIT 1",
                params![input.invoice_id, input.idem_key],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| Database(e.to_string()))?
        {
            tx.commit().map_err(|e| Database(e.to_string()))?;
            return Ok(RefundCreation {
                id: existing,
                created_new: false,
            });
        }

        // Fall back to the invoice's buyer when none was supplied, and load
        // the fields needed for the cap / asset lock in the same snapshot.
        let mut user_id = input.user_id;
        if let Some(inv) = input.invoice_id {
            let (inv_user, inv_amount, inv_asset, inv_status): (
                Option<i64>,
                String,
                String,
                String,
            ) = tx
                .query_row(
                    "SELECT user_id,amount,asset,status FROM invoices WHERE invoice_id=?1",
                    params![inv],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(|e| Database(e.to_string()))?
                .ok_or(InvoiceNotFound)?;
            if user_id.is_none() {
                user_id = inv_user;
            }
            // Decision 3 keeps paid_ungranted non-terminal and its auto-refund
            // must be creatable while the invoice is still in that state.
            if inv_status != "paid" && inv_status != "paid_ungranted" {
                return Err(InvoiceNotPaid);
            }
            if input.asset.trim() != inv_asset.trim() {
                return Err(AssetMismatch);
            }

            let invoice_minor = Self::minor_units(&inv_amount).ok_or(UnquantizableLegacy)?;
            let requested = if input.amount.trim().is_empty() {
                invoice_minor
            } else {
                Self::minor_units(input.amount).ok_or(UnquantizableLegacy)?
            };
            // Counted states: every row except 'void' — including terminal
            // executed/processed (a reverted attempt is still spent once).
            let mut counted: Vec<String> = tx
                .prepare("SELECT amount FROM refunds WHERE invoice_id=?1 AND status <> 'void'")
                .and_then(|mut stmt| {
                    stmt.query_map(params![inv], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(|e| Database(e.to_string()))?;
            let mut cumulative: i128 = 0;
            for row in counted.drain(..) {
                let units = Self::minor_units(&row).ok_or(UnquantizableLegacy)?;
                cumulative = cumulative.checked_add(units).ok_or(UnquantizableLegacy)?;
            }
            let total = cumulative
                .checked_add(requested)
                .ok_or(UnquantizableLegacy)?;
            if total > invoice_minor {
                return Err(CapExceeded);
            }

            let in_flight: Option<i64> = tx
                .query_row(
                    "SELECT id FROM refunds WHERE invoice_id=?1 \
                     AND status IN ('pending','approved','executing','needs_reconciliation') LIMIT 1",
                    params![inv],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| Database(e.to_string()))?;
            if in_flight.is_some() {
                return Err(InFlight);
            }
        }

        let insert = tx.execute(
            "INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,reason,status,idem_key,created_by,created_role,created_at,updated_at) \
             VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?8,?9,?10,?10)",
            params![
                input.seller_id,
                input.invoice_id,
                user_id,
                input.amount,
                input.asset,
                input.reason,
                input.idem_key,
                input.created_by,
                input.created_role,
                now
            ],
        );
        let id = match insert {
            Ok(_) => tx.last_insert_rowid(),
            Err(e) => {
                // A concurrent creator can win either unique index between
                // our read and our write; translate that race to the same
                // stable outcome instead of leaking a rusqlite error.
                let msg = e.to_string();
                if msg.contains("uq_refunds_invoice_idem") {
                    let existing = tx
                        .query_row(
                            "SELECT id FROM refunds WHERE invoice_id IS ?1 AND idem_key=?2 ORDER BY id LIMIT 1",
                            params![input.invoice_id, input.idem_key],
                            |r| r.get::<_, i64>(0),
                        )
                        .optional()
                        .map_err(|inner| Database(inner.to_string()))?;
                    if let Some(id) = existing {
                        tx.commit().map_err(|inner| Database(inner.to_string()))?;
                        return Ok(RefundCreation {
                            id,
                            created_new: false,
                        });
                    }
                }
                if msg.contains("uq_refunds_invoice_inflight") {
                    return Err(InFlight);
                }
                return Err(Database(msg));
            }
        };
        tx.commit().map_err(|e| Database(e.to_string()))?;
        Ok(RefundCreation {
            id,
            created_new: true,
        })
    }

    /// (id, seller_id, invoice_id, user_id, amount, asset, reason, status)
    #[allow(clippy::type_complexity)] // row tuple mirrors the table shape
    pub fn list_refunds(
        &self,
        seller_scope: Option<i64>,
    ) -> Vec<(
        i64,
        i64,
        Option<String>,
        Option<i64>,
        String,
        String,
        Option<String>,
        String,
    )> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT id,seller_id,invoice_id,user_id,amount,asset,reason,status FROM refunds WHERE seller_id=?1 ORDER BY id DESC".to_string(), vec![Box::new(sid)])
        } else {
            (
                "SELECT id,seller_id,invoice_id,user_id,amount,asset,reason,status FROM refunds ORDER BY id DESC"
                    .to_string(),
                vec![],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_refund(&self, id: impl ToString) -> Option<RefundRow> {
        let id = id.to_string().parse::<i64>().ok()?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,invoice_id,user_id,amount,asset,reason,status,created_at,updated_at,processed_at,processed_by FROM refunds WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?)),
        ).optional().ok().flatten()
    }

    /// Persisted creator actor for a refund: (created_by, created_role).
    /// Reads the authoritative columns written by `create_refund_guarded`,
    /// which — unlike the `refund_create` audit event — never age out of the
    /// audit window on a mature DB (§2 separation of duties).
    pub fn refund_creator(&self, id: i64) -> Option<(i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT created_by, created_role FROM refunds WHERE id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn update_refund_status(&self, id: impl ToString, status: impl AsRef<str>) {
        self.update_refund_status_by(id, status, None);
    }
    pub fn update_refund_status_by(
        &self,
        id: impl ToString,
        status: impl AsRef<str>,
        processed_by: Option<i64>,
    ) {
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let st = status.as_ref();
        let final_status = matches!(st, "processed" | "paid" | "failed");
        if final_status {
            let _ = conn.execute(
                "UPDATE refunds SET status=?1, updated_at=?2, processed_at=?2, processed_by=COALESCE(?3, processed_by) WHERE id=?4",
                params![st, now, processed_by, id],
            );
        } else {
            let _ = conn.execute(
                "UPDATE refunds SET status=?1, updated_at=?2, processed_by=COALESCE(?3, processed_by) WHERE id=?4",
                params![st, now, processed_by, id],
            );
        }
    }

    /// v1.2 refund state machine (pending -> approved -> executed | rejected).
    pub fn set_refund_status(&self, id: i64, status: &str) {
        self.update_refund_status(id, status);
    }

    /// Atomically claim an approved refund for execution: flips
    /// 'approved' -> 'executing' only if the row is still 'approved', so two
    /// concurrent executors can never both win the CAS (the loser sees false
    /// and must not transfer). The claim stamps `updated_at`. This is the
    /// ONLY sanctioned way to enter the 'executing' state — callers must
    /// never write 'executing' directly (same contract as
    /// claim_invoice_for_grant).
    pub fn begin_refund_execution(&self, id: impl ToString) -> bool {
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE refunds SET status='executing', updated_at=?2, executing_since=?2 \
             WHERE id=?1 AND status='approved'",
            params![id, now],
        ) == Ok(1)
    }

    /// Settle a claimed refund: flips 'executing' -> `status`, only from
    /// 'executing'. `status` must be "executed" (definite success) or
    /// "approved" (definite failure: revert so the refund can be re-executed);
    /// anything else — including a still-ambiguous outcome — is rejected with
    /// no DB write and returns 0 (leave the row 'executing' and let a fresh
    /// audit row carry the ambiguity instead). Returns the number of rows
    /// changed: 1 means this call owned the transition.
    pub fn finish_refund_execution(&self, id: impl ToString, status: &str) -> usize {
        if !matches!(status, "executed" | "approved") {
            return 0;
        }
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE refunds SET status=?2, updated_at=?3 WHERE id=?1 AND status='executing'",
            params![id, status, now],
        )
        .unwrap_or(0)
    }

    // ---- Payouts ----
    pub fn create_payout(
        &self,
        seller_id: i64,
        amount: &str,
        asset: &str,
        destination: Option<&str>,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO payouts(seller_id,amount,asset,destination,status,created_at,updated_at) VALUES(?1,?2,?3,?4,'pending',?5,?5)",
            params![seller_id, amount, asset, destination, now],
        );
        conn.last_insert_rowid()
    }

    pub fn list_payouts(
        &self,
        seller_scope: Option<i64>,
    ) -> Vec<(i64, i64, String, String, Option<String>, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT id,seller_id,amount,asset,destination,status FROM payouts WHERE seller_id=?1 ORDER BY id DESC".to_string(), vec![Box::new(sid)])
        } else {
            (
                "SELECT id,seller_id,amount,asset,destination,status FROM payouts ORDER BY id DESC"
                    .to_string(),
                vec![],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn get_payout(&self, id: impl ToString) -> Option<PayoutRow> {
        let id = id.to_string().parse::<i64>().ok()?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,amount,asset,destination,status,created_at,updated_at,processed_at,processed_by FROM payouts WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?)),
        ).optional().ok().flatten()
    }

    pub fn update_payout_status(&self, id: impl ToString, status: impl AsRef<str>) {
        self.update_payout_status_by(id, status, None);
    }
    pub fn update_payout_status_by(
        &self,
        id: impl ToString,
        status: impl AsRef<str>,
        processed_by: Option<i64>,
    ) {
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let st = status.as_ref();
        let final_status = matches!(st, "paid" | "failed" | "processed");
        if final_status {
            let _ = conn.execute(
                "UPDATE payouts SET status=?1, updated_at=?2, processed_at=?2, processed_by=COALESCE(?3, processed_by) WHERE id=?4",
                params![st, now, processed_by, id],
            );
        } else {
            let _ = conn.execute(
                "UPDATE payouts SET status=?1, updated_at=?2, processed_by=COALESCE(?3, processed_by) WHERE id=?4",
                params![st, now, processed_by, id],
            );
        }
    }

    /// v1.2 payout state machine (pending -> approved -> paid).
    pub fn set_payout_status(&self, id: i64, status: &str) {
        self.update_payout_status(id, status);
    }

    /// Atomically claim an approved payout for execution: flips
    /// 'approved' -> 'executing' only if the row is still 'approved', so two
    /// concurrent executors can never both win the CAS (the loser sees false
    /// and must not transfer — the double-transfer fix). The claim stamps
    /// `updated_at`. This is the ONLY sanctioned way to enter the 'executing'
    /// state — callers must never write 'executing' directly (same contract
    /// as claim_invoice_for_grant).
    pub fn begin_payout_execution(&self, id: impl ToString) -> bool {
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return false,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE payouts SET status='executing', updated_at=?2, executing_since=?2 \
             WHERE id=?1 AND status='approved'",
            params![id, now],
        ) == Ok(1)
    }

    /// Settle a claimed payout: flips 'executing' -> `status`, only from
    /// 'executing'. `status` must be "paid" (definite success) or "approved"
    /// (definite failure: revert so the payout can be re-executed); anything
    /// else — including a still-ambiguous outcome — is rejected with no DB
    /// write and returns 0 (leave the row 'executing' and let a fresh audit
    /// row carry the ambiguity instead). Returns the number of rows changed:
    /// 1 means this call owned the transition.
    pub fn finish_payout_execution(&self, id: impl ToString, status: &str) -> usize {
        if !matches!(status, "paid" | "approved") {
            return 0;
        }
        let id = match id.to_string().parse::<i64>() {
            Ok(v) => v,
            Err(_) => return 0,
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE payouts SET status=?2, updated_at=?3 WHERE id=?1 AND status='executing'",
            params![id, status, now],
        )
        .unwrap_or(0)
    }

    /// Current status of a payout, for executors and recovery sweeps to
    /// distinguish a row someone still owns ('executing') from one already
    /// settled ('paid') or reverted ('approved').
    pub fn get_payout_status(&self, id: impl ToString) -> Option<String> {
        let id = id.to_string().parse::<i64>().ok()?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row("SELECT status FROM payouts WHERE id=?1", params![id], |r| {
            r.get(0)
        })
        .optional()
        .ok()
        .flatten()
    }

    // ---- Money safety: spend-id ledger + reconciliation (§4 / §5) ----

    /// §4.1: persist a FRESH spend_id in a committed transaction BEFORE any
    /// transfer call. `attempt_no` increments monotonically per
    /// (row_type, row_id) inside the same immediate transaction so two
    /// concurrent attempts cannot claim the same number. A duplicate
    /// spend_id is a hard error — never an overwrite. Returns the new row id.
    pub fn record_transfer_attempt(
        &self,
        row_type: &str,
        row_id: i64,
        seller_id: i64,
        spend_id: &str,
    ) -> Result<i64, rusqlite::Error> {
        if !matches!(row_type, "refund" | "payout") || spend_id.trim().is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let mut conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let last: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(attempt_no),0) FROM transfer_attempts \
                 WHERE row_type=?1 AND row_id=?2",
                params![row_type, row_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let now = Utc::now().to_rfc3339();
        let attempt_no = last
            .checked_add(1)
            .ok_or_else(|| Self::sqlite_fail("transfer attempt number overflow"))?;
        tx.execute(
            "INSERT INTO transfer_attempts(row_type,row_id,attempt_no,spend_id,created_at,seller_id) \
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![row_type, row_id, attempt_no, spend_id, now, seller_id],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }

    /// Look up an attempt by its globally unique spend_id — used by the
    /// ambiguous-retry path (§4.1) to reuse the SAME key.
    pub fn get_transfer_attempt(&self, spend_id: &str) -> Option<TransferAttemptRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,row_type,row_id,attempt_no,spend_id,created_at,outcome,resolved_at,seller_id \
             FROM transfer_attempts WHERE spend_id=?1",
            params![spend_id],
            |r| {
                Ok(TransferAttemptRow {
                    id: r.get(0)?,
                    row_type: r.get(1)?,
                    row_id: r.get(2)?,
                    attempt_no: r.get(3)?,
                    spend_id: r.get(4)?,
                    created_at: r.get(5)?,
                    outcome: r.get(6)?,
                    resolved_at: r.get(7)?,
                    seller_id: r.get(8)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// The most recent attempt for a money row (highest attempt_no). The
    /// digest (§5.4) attaches this spend_id to executing/reconciling rows.
    pub fn latest_transfer_attempt(
        &self,
        row_type: &str,
        row_id: i64,
    ) -> Option<TransferAttemptRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,row_type,row_id,attempt_no,spend_id,created_at,outcome,resolved_at,seller_id \
             FROM transfer_attempts WHERE row_type=?1 AND row_id=?2 \
             ORDER BY attempt_no DESC, id DESC LIMIT 1",
            params![row_type, row_id],
            |r| {
                Ok(TransferAttemptRow {
                    id: r.get(0)?,
                    row_type: r.get(1)?,
                    row_id: r.get(2)?,
                    attempt_no: r.get(3)?,
                    spend_id: r.get(4)?,
                    created_at: r.get(5)?,
                    outcome: r.get(6)?,
                    resolved_at: r.get(7)?,
                    seller_id: r.get(8)?,
                })
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// §4.2 outcome persistence. Callers branch on the shared classifier
    /// enum (server); the data layer only stores the token and stamps the
    /// resolution clock. A missing spend_id is a no-op (never an overwrite).
    pub fn set_transfer_attempt_outcome(&self, spend_id: &str, outcome: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE transfer_attempts SET outcome=?2, resolved_at=?3 WHERE spend_id=?1",
            params![spend_id, outcome, now],
        ) == Ok(1)
    }

    /// §5.1: `executing` older than the injected cutoff -> `needs_reconciliation`.
    /// No transfer is attempted and no state is guessed. The spend-id rows and
    /// `executing_since` stay attached; `reconciliation_since` starts the 24h
    /// owner-decision clock. CAS on the exact status, so a concurrent finish
    /// can never be overwritten by the sweeper.
    pub fn mark_needs_reconciliation(
        &self,
        row_type: &str,
        id: i64,
        executing_before: &str,
    ) -> bool {
        if !matches!(row_type, "refund" | "payout") {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let table = if row_type == "refund" {
            "refunds"
        } else {
            "payouts"
        };
        // `table` is chosen from a fixed two-value match above, never input.
        let sql = format!(
            "UPDATE {table} SET status='needs_reconciliation', \
             updated_at=?3, reconciliation_since=?3 \
             WHERE id=?1 AND status='executing' \
               AND executing_since IS NOT NULL AND executing_since <= ?2"
        );
        conn.execute(&sql, params![id, executing_before, now]) == Ok(1)
    }

    /// §5.4 digest input: every `executing` row past the due cutoff and every
    /// `needs_reconciliation` row (with its overdue flag past the decision
    /// cutoff), both money types, with the latest persisted spend_id. All
    /// timestamps are injected so verification can test 30min/24h boundaries.
    #[allow(clippy::type_complexity)]
    pub fn reconciliation_rows(
        &self,
        executing_cutoff: &str,
        overdue_cutoff: &str,
    ) -> Vec<ReconciliationRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut out = Vec::new();
        for row_type in ["refund", "payout"] {
            let table = if row_type == "refund" {
                "refunds"
            } else {
                "payouts"
            };
            // `table`/`row_type` are fixed literals from the loop above.
            let sql = format!(
                "SELECT m.id,m.seller_id,m.amount,m.asset,m.status, \
                 COALESCE(m.reconciliation_since,m.executing_since,'') AS since, \
                 (SELECT t.spend_id FROM transfer_attempts t \
                   WHERE t.row_type=?1 AND t.row_id=m.id \
                   ORDER BY t.attempt_no DESC, t.id DESC LIMIT 1) AS spend_id \
                 FROM {table} m \
                 WHERE (m.status='executing' AND m.executing_since IS NOT NULL \
                          AND m.executing_since <= ?2) \
                    OR (m.status='needs_reconciliation' \
                          AND COALESCE(m.reconciliation_since,m.executing_since,'') <= ?3) \
                 ORDER BY m.id"
            );
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let rows = stmt.query_map(params![row_type, executing_cutoff, overdue_cutoff], |r| {
                let since: String = r.get(5)?;
                Ok(ReconciliationRow {
                    row_type: row_type.to_string(),
                    id: r.get(0)?,
                    seller_id: r.get(1)?,
                    amount: r.get(2)?,
                    asset: r.get(3)?,
                    status: r.get(4)?,
                    overdue: since.as_str() <= overdue_cutoff,
                    since,
                    spend_id: r.get(6)?,
                })
            });
            if let Ok(iter) = rows {
                out.extend(iter.filter_map(|r| r.ok()));
            }
        }
        out
    }

    /// §5.3 owner resolution. Exactly three outcomes; append-only (a second
    /// resolution is rejected by the `resolution IS NULL` CAS and the
    /// append-only trigger). `resolution` stores the human reason; the
    /// resulting status stores the outcome token; `resolved_by`/`resolved_at`
    /// record the auditor. The caller (server) writes the audit row with
    /// spend_id + actor + reason.
    pub fn resolve_reconciliation(
        &self,
        row_type: &str,
        id: i64,
        outcome: ReconciliationOutcome,
        resolved_by: i64,
        reason: &str,
    ) -> Result<(), ReconcileError> {
        use ReconcileError::*;
        if !matches!(row_type, "refund" | "payout") {
            return Err(NotFound);
        }
        if reason.trim().is_empty() {
            return Err(ReasonRequired);
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let existing: (String, Option<String>) = conn
            .query_row(
                &format!(
                    "SELECT status,resolution FROM {} WHERE id=?1",
                    if row_type == "refund" {
                        "refunds"
                    } else {
                        "payouts"
                    }
                ),
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| Database(e.to_string()))?
            .ok_or(NotFound)?;
        if existing.1.is_some() {
            return Err(AlreadyResolved);
        }
        if existing.0 != "needs_reconciliation" {
            return Err(NotDue);
        }
        let status = outcome.status_for(row_type);
        let now = Utc::now().to_rfc3339();
        let table = if row_type == "refund" {
            "refunds"
        } else {
            "payouts"
        };
        let changed = conn
            .execute(
                &format!(
                    "UPDATE {table} SET status=?2, resolution=?3, resolved_by=?4, \
                     resolved_at=?5, updated_at=?5 \
                     WHERE id=?1 AND status='needs_reconciliation' AND resolution IS NULL"
                ),
                params![id, status, reason.trim(), resolved_by, now],
            )
            .map_err(|e| Database(e.to_string()))?;
        if changed == 0 {
            return Err(AlreadyResolved);
        }
        Ok(())
    }

    /// Persisted resolution of a money row, if any:
    /// (outcome-status, reason, resolved_by, resolved_at).
    pub fn money_resolution(
        &self,
        row_type: &str,
        id: i64,
    ) -> Option<(String, String, i64, String)> {
        let table = if row_type == "refund" {
            "refunds"
        } else if row_type == "payout" {
            "payouts"
        } else {
            return None;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            &format!(
                "SELECT status,resolution,resolved_by,resolved_at FROM {table} \
                 WHERE id=?1 AND resolution IS NOT NULL"
            ),
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    // ---- Audit log ----
    pub fn log_action(
        &self,
        actor_id: i64,
        actor_role: impl AsRef<str>,
        action: impl AsRef<str>,
        target_type: impl AsRef<str>,
        target_id: impl ToString,
        metadata: Option<&str>,
    ) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO audit_log(actor_id,actor_role,action,target_type,target_id,metadata,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![actor_id, actor_role.as_ref(), action.as_ref(), target_type.as_ref(), target_id.to_string(), metadata, now],
        );
    }

    pub fn list_audit_log(&self, limit: i64) -> Vec<AuditLogRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT id,actor_id,actor_role,action,target_type,target_id,metadata,created_at FROM audit_log ORDER BY id DESC LIMIT ?1") { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- API keys ----
    pub fn create_api_key(&self, account_id: i64, name: Option<&str>) -> String {
        self.create_api_key_full(account_id, name, None).1
    }

    pub fn create_api_key_full(
        &self,
        account_id: i64,
        name: Option<&str>,
        scopes: Option<&str>,
    ) -> (i64, String) {
        let raw = crate::util::gen_token(48);
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(raw.as_bytes());
            let bytes = h.finalize();
            let mut s = String::with_capacity(64);
            for b in bytes.iter() {
                s.push_str(&format!("{:02x}", b));
            }
            s
        };
        let prefix = raw.chars().take(8).collect::<String>();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO api_keys(account_id,name,key_hash,key_prefix,scopes,revoked,created_at) VALUES(?1,?2,?3,?4,?5,0,?6)",
            params![account_id, name, hash, prefix, scopes, now],
        );
        (conn.last_insert_rowid(), raw)
    }

    pub fn get_api_key_by_hash(&self, hash: impl AsRef<str>) -> Option<ApiKeyRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,account_id,name,key_hash,scopes,revoked,created_at FROM api_keys WHERE key_hash=?1",
            params![hash.as_ref()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional().ok().flatten()
    }

    pub fn get_mcp_token_by_hash(
        &self,
        hash: &str,
    ) -> Option<(i64, i64, Option<String>, i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id, seller_id, name, revoked, expires_at FROM mcp_tokens WHERE token_hash=?1",
            params![hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Mint a per-seller MCP bearer token (D6): `gen_token(48)` stored
    /// SHA-256-hashed with an 8-char display prefix — the create_api_key_full
    /// convention verbatim, including the raw-return-once pattern (only the
    /// hash and prefix are persisted; the raw token exists in this return
    /// value and nowhere else).
    pub fn create_mcp_token(&self, seller_id: i64, name: Option<&str>) -> (i64, String) {
        let raw = crate::util::gen_token(48);
        let hash = sha256_hex(&raw);
        let prefix = raw.chars().take(8).collect::<String>();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO mcp_tokens(seller_id,token_hash,token_prefix,name,revoked,created_at) VALUES(?1,?2,?3,?4,0,?5)",
            params![seller_id, hash, prefix, name, now],
        );
        (conn.last_insert_rowid(), raw)
    }

    /// A seller's own tokens, masked (D6 `/mcptoken list`): rows of
    /// (id, name, token_prefix, created_at, revoked) — never the hash, never
    /// the raw token.
    pub fn list_mcp_tokens(
        &self,
        seller_id: i64,
    ) -> Vec<(i64, Option<String>, Option<String>, String, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id, name, token_prefix, created_at, revoked FROM mcp_tokens WHERE seller_id=?1 ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![seller_id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Revoke one of the seller's tokens (D6 `/mcptoken revoke`), scoped like
    /// every seller write: `Some(seller)` only touches that seller's own rows,
    /// `None` acts on any row. Idempotent — an already-revoked row still
    /// reports true; only unknown ids / wrong scope report false.
    pub fn revoke_mcp_token(&self, id: i64, seller_scope: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE mcp_tokens SET revoked=1 WHERE id=?1 AND (?2 IS NULL OR seller_id=?2)",
            params![id, seller_scope],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Stamp last_used_at on a token after a successful authenticated call
    /// (best-effort by contract: the auth path must not fail because
    /// bookkeeping did).
    pub fn touch_mcp_token(&self, id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = conn.execute(
            "UPDATE mcp_tokens SET last_used_at=?1 WHERE id=?2",
            params![Utc::now().to_rfc3339(), id],
        );
    }

    pub fn get_api_key_by_id(&self, id: i64) -> Option<ApiKeyRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id, account_id, name, key_prefix, scopes, revoked, created_at FROM api_keys WHERE id=?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        ).optional().ok().flatten()
    }

    pub fn list_api_keys(&self, account_id: i64) -> Vec<ApiKeyListRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT id,account_id,name,key_prefix,revoked,created_at FROM api_keys WHERE account_id=?1 ORDER BY id DESC") { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn list_api_keys_detail(&self, account_id: i64) -> Vec<ApiKeyRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT id,account_id,name,key_prefix,scopes,revoked,created_at FROM api_keys WHERE account_id=?1 ORDER BY id DESC") { Ok(s)=>s, Err(_)=>return vec![] };
        let rows = stmt
            .query_map(params![account_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn revoke_api_key(&self, id: impl ToString) {
        let s = id.to_string();
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Ok(nid) = s.parse::<i64>() {
            let _ = conn.execute("UPDATE api_keys SET revoked=1 WHERE id=?1", params![nid]);
            return;
        }
        // try as raw key -> hash
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(s.as_bytes());
            let bytes = h.finalize();
            let mut out = String::with_capacity(64);
            for b in bytes.iter() {
                out.push_str(&format!("{:02x}", b));
            }
            out
        };
        let _ = conn.execute(
            "UPDATE api_keys SET revoked=1 WHERE key_hash=?1 OR key_prefix=?2",
            params![hash, s],
        );
    }

    // ---- Stats ----
    pub fn count_sellers(&self) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row("SELECT COUNT(*) FROM sellers", [], |r| r.get(0))
            .unwrap_or(0)
    }
    pub fn count_users(&self) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap_or(0)
    }
    pub fn count_plans(&self, seller_scope: Option<i64>) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sid) = seller_scope {
            conn.query_row(
                "SELECT COUNT(*) FROM plans WHERE seller_id=?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0)
        } else {
            conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get(0))
                .unwrap_or(0)
        }
    }
    pub fn count_subscriptions(&self, seller_scope: Option<i64>, status_opt: Option<&str>) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = "SELECT COUNT(*) FROM subscriptions".to_string();
        let mut conds = vec![];
        if seller_scope.is_some() {
            conds.push("seller_id=?1".to_string());
        }
        if status_opt.is_some() {
            if seller_scope.is_some() {
                conds.push("status=?2".to_string());
            } else {
                conds.push("status=?1".to_string());
            }
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let res = match (seller_scope, status_opt) {
            (Some(sid), Some(st)) => stmt.query_row(params![sid, st], |r| r.get(0)),
            (Some(sid), None) => stmt.query_row(params![sid], |r| r.get(0)),
            (None, Some(st)) => stmt.query_row(params![st], |r| r.get(0)),
            (None, None) => stmt.query_row([], |r| r.get(0)),
        };
        res.unwrap_or(0)
    }

    /// (status, count) for every status present, server-side GROUP BY so the
    /// MCP subs_report aggregates without pulling user rows.
    pub fn subscription_status_counts(&self, seller_scope: Option<i64>) -> Vec<(String, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) =
            if let Some(sid) = seller_scope {
                (
                    "SELECT status, COUNT(*) FROM subscriptions WHERE seller_id=?1 GROUP BY status"
                        .to_string(),
                    vec![Box::new(sid)],
                )
            } else {
                (
                    "SELECT status, COUNT(*) FROM subscriptions GROUP BY status".to_string(),
                    vec![],
                )
            };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        );
        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    pub fn count_invoices(&self, seller_scope: Option<i64>, status_opt: Option<&str>) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = "SELECT COUNT(*) FROM invoices".to_string();
        let mut conds = vec![];
        if seller_scope.is_some() {
            conds.push("seller_id=?1".to_string());
        }
        if status_opt.is_some() {
            if seller_scope.is_some() {
                conds.push("status=?2".to_string());
            } else {
                conds.push("status=?1".to_string());
            }
        }
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        let res = match (seller_scope, status_opt) {
            (Some(sid), Some(st)) => stmt.query_row(params![sid, st], |r| r.get(0)),
            (Some(sid), None) => stmt.query_row(params![sid], |r| r.get(0)),
            (None, Some(st)) => stmt.query_row(params![st], |r| r.get(0)),
            (None, None) => stmt.query_row([], |r| r.get(0)),
        };
        res.unwrap_or(0)
    }
    pub fn count_codes(&self, seller_scope: Option<i64>) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sid) = seller_scope {
            conn.query_row(
                "SELECT COUNT(*) FROM activation_codes WHERE seller_id=?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0)
        } else {
            conn.query_row("SELECT COUNT(*) FROM activation_codes", [], |r| r.get(0))
                .unwrap_or(0)
        }
    }
    pub fn count_posts(&self, seller_scope: Option<i64>) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(sid) = seller_scope {
            conn.query_row(
                "SELECT COUNT(*) FROM posts WHERE seller_id=?1",
                params![sid],
                |r| r.get(0),
            )
            .unwrap_or(0)
        } else {
            conn.query_row("SELECT COUNT(*) FROM posts", [], |r| r.get(0))
                .unwrap_or(0)
        }
    }
    /// Paid revenue per asset, optionally scoped to one seller. This is the
    /// correct source for every money display; each amount stays in its own
    /// denomination, so a BTC amount is never added to a USDT amount.
    pub fn revenue_by_asset_scoped(&self, seller_scope: Option<i64>) -> Vec<(String, f64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sql = if seller_scope.is_some() {
            "SELECT asset, COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE seller_id=?1 AND status='paid' GROUP BY asset ORDER BY asset"
        } else {
            "SELECT asset, COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE status='paid' GROUP BY asset ORDER BY asset"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        if let Some(sid) = seller_scope {
            match stmt.query_map(params![sid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            }) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(_) => vec![],
            }
        } else {
            match stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))) {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(_) => vec![],
            }
        }
    }

    /// Paid revenue per asset across every seller. See the finance ruling in
    /// `docs/CRYPTO_PAY.md`: cross-asset summing is a display defect.
    pub fn revenue_by_asset(&self) -> Vec<(String, f64)> {
        self.revenue_by_asset_scoped(None)
    }

    /// Single-denomination total, for the common case where every paid
    /// invoice uses the same asset. Returns `Err(MixedAssets)` as soon as a
    /// second asset appears — the caller must show per-asset lines instead.
    pub fn revenue_total(&self, seller_scope: Option<i64>) -> Result<f64, MixedAssets> {
        let rows = self.revenue_by_asset_scoped(seller_scope);
        match rows.len() {
            0 => Ok(0.0),
            1 => Ok(rows[0].1),
            _ => Err(MixedAssets),
        }
    }

    /// Per-seller revenue as per-asset lines. A seller selling two assets has
    /// two entries; no cross-asset number is produced anywhere.
    pub fn revenue_by_seller(&self) -> Vec<(i64, Vec<(String, f64)>)> {
        self.list_sellers()
            .into_iter()
            .map(|sid| (sid, self.revenue_by_asset_scoped(Some(sid))))
            .collect()
    }

    // ---- MCP approvals & audit ----
    /// Park a sensitive tool call: row starts 'pending' and is executed only
    /// after the owner approves it in the bot. Returns the approval id.
    pub fn mcp_create_approval(&self, tool: &str, args_json: &str, expires_at: &str) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO mcp_approvals(tool,args_json,status,created_at,expires_at) VALUES(?1,?2,'pending',?3,?4)",
            params![tool, args_json, now, expires_at],
        );
        conn.last_insert_rowid()
    }

    pub fn mcp_get_approval(&self, id: i64) -> Option<McpApprovalRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,tool,args_json,status,created_at,expires_at,decided_at,executed_at FROM mcp_approvals WHERE id=?1",
            params![id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten()
    }

    /// Transition a pending approval to `status` ('approved'/'denied'/'expired').
    /// The `status='pending'` guard makes the decide atomic: concurrent or
    /// repeated decides lose the rowcount race and return false, so a
    /// double-approve can never execute twice.
    pub fn mcp_decide_approval(&self, id: i64, status: &str) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let n = conn
            .execute(
                "UPDATE mcp_approvals SET status=?1, decided_at=?2 WHERE id=?3 AND status='pending'",
                params![status, now, id],
            )
            .unwrap_or(0);
        n > 0
    }

    pub fn mcp_mark_executed(&self, id: i64) {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE mcp_approvals SET executed_at=?1 WHERE id=?2",
            params![now, id],
        );
    }

    pub fn mcp_list_approvals(&self, status: Option<&str>, limit: i64) -> Vec<McpApprovalRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Lazy purge: pendings past their expiry flip to 'expired' on every
        // list, so /approvals never offers a stale row for approval.
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE mcp_approvals SET status='expired', decided_at=?1 WHERE status='pending' AND expires_at < ?1",
            params![now],
        );
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(st) = status {
            // Pending list reads oldest-first so the owner sees the longest
            // waiting request at the top; terminal lists read newest-first.
            let order = if st == "pending" { "ASC" } else { "DESC" };
            (
                format!(
                    "SELECT id,tool,args_json,status,created_at,expires_at,decided_at,executed_at FROM mcp_approvals WHERE status=?1 ORDER BY id {order} LIMIT ?2"
                ),
                vec![Box::new(st.to_string()), Box::new(limit)],
            )
        } else {
            (
                "SELECT id,tool,args_json,status,created_at,expires_at,decided_at,executed_at FROM mcp_approvals ORDER BY id DESC LIMIT ?1".to_string(),
                vec![Box::new(limit)],
            )
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// One row per MCP tool-call outcome. `detail` is truncated to 200 chars
    /// here (the single choke point) so an over-verbose caller can never leak
    /// a large payload — and generated codes must never be passed as detail.
    /// One row per MCP tool-call outcome (v1.4 D6): `seller_id` records which
    /// seller principal ran the tool (owner rows stay NULL) so "which token
    /// did what" is answerable. Detail is truncated here so secrets never
    /// land in the audit trail.
    pub fn mcp_audit(
        &self,
        tool: &str,
        role: &str,
        seller_id: Option<i64>,
        ok: bool,
        detail: Option<&str>,
    ) {
        let detail = detail.map(|d| d.chars().take(200).collect::<String>());
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO mcp_audit(tool,role,seller_id,ok,detail,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![tool, role, seller_id, ok, detail, now],
        );
    }

    pub fn mcp_list_audit(&self, limit: i64) -> Vec<McpAuditRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,tool,role,ok,detail,created_at FROM mcp_audit ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    // ---- Append-only additions for the server API/observability task
    // (2026-10-25). Kept as a clearly-marked block at the end of the impl so
    // parallel edits elsewhere in db.rs cannot collide; no existing lines
    // were modified. Lock handling follows the poisoned-mutex recovery
    // pattern used by the mcp_* methods above.

    /// Cheap liveness probe for `/health`: `SELECT 1`. Returns false on any
    /// error so the health endpoint can report `db: "error"` honestly
    /// instead of panicking. (server addition)
    pub fn ping(&self) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))
            .map(|v| v == 1)
            .unwrap_or(false)
    }

    /// Invalidate every web session of `account_id` except the one for
    /// `keep_token` (hashed here, matching `create_web_session`/
    /// `get_web_session`). Used by the API password change so a password
    /// rotation evicts stolen sessions but keeps the caller logged in.
    /// (server addition)
    pub fn delete_other_web_sessions(&self, account_id: i64, keep_token: &str) {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(keep_token.as_bytes());
        let bytes = h.finalize();
        let mut hash = String::with_capacity(64);
        for b in bytes.iter() {
            hash.push_str(&format!("{:02x}", b));
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "DELETE FROM web_sessions WHERE account_id=?1 AND token_hash != ?2",
            params![account_id, hash],
        );
    }

    /// Delete an activation code, scoped: `seller_id = Some(id)` only deletes
    /// codes owned by that seller, `None` (admin/owner) deletes any. Returns
    /// true when a row was deleted. Redemption rows for the deleted code are
    /// removed too — and only after the scope predicate matched, so a
    /// cross-seller attempt deletes nothing at all. (server addition)
    pub fn delete_code_seller_scoped(&self, code: &str, seller_id: Option<i64>) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        // Match first (with the scope predicate), then delete the row and its
        // redemptions by id — a scope mismatch must not touch anything.
        let matched: Vec<i64> = match conn.prepare(
            "SELECT id FROM activation_codes WHERE code=?1 AND (?2 IS NULL OR seller_id=?2)",
        ) {
            Ok(mut stmt) => {
                let mapped = stmt.query_map(params![code, seller_id], |r| r.get::<_, i64>(0));
                match mapped {
                    Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                    Err(_) => return false,
                }
            }
            Err(_) => return false,
        };
        let mut deleted = false;
        for id in matched {
            let redemptions =
                conn.execute("DELETE FROM code_redemptions WHERE code_id=?1", params![id]);
            let code_row = conn
                .execute("DELETE FROM activation_codes WHERE id=?1", params![id])
                .map(|n| n > 0)
                .unwrap_or(false);
            if redemptions.is_ok() && code_row {
                deleted = true;
            }
        }
        deleted
    }

    // ---- Channels store (v1.3.x: "channels become managed data") ----
    //
    // `seller_id` 0 is the global namespace — the convention the plan
    // namespace work established. Channel names are unique console-wide
    // (UNIQUE COLLATE NOCASE in schema.sql): a Telegram @username names one
    // channel no matter who references it, so dedupe errors are honest, not
    // incidental. The legacy free-text `channel` config key is NOT removed;
    // it survives only as `default_channel`'s fallback for legacy installs.

    fn map_channel_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelRow> {
        Ok(ChannelRow {
            id: r.get(0)?,
            channel: r.get(1)?,
            title: r.get(2)?,
            seller_id: r.get(3)?,
            is_default: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
        })
    }

    /// Register `channel` under `seller_id` (0 = global/owner). Fails with an
    /// honest message on the table-wide UNIQUE (the name already exists, in
    /// any namespace, any letter case) and on empty names.
    pub fn add_channel(&self, channel: &str, seller_id: i64) -> Result<(), String> {
        let ch = channel.trim();
        if ch.is_empty() {
            return Err("channel must not be empty".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO channels(channel,seller_id,created_at) VALUES(?1,?2,?3)",
            params![ch, seller_id, now],
        )
        .map(|_| ())
        .map_err(|e| {
            if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
                format!("channel '{ch}' already exists")
            } else {
                format!("failed to add channel '{ch}': {e}")
            }
        })
    }

    /// Rename a channel row scoped to `seller_id` (0 = global namespace):
    /// only the row owned by this seller and matching `old_name`
    /// (case-insensitively, per the column's NOCASE collation) is renamed to
    /// `new_name`. The `is_default` flag is left untouched, so a default
    /// channel keeps being the default under its new name. Returns an honest
    /// Err when the row is missing / not owned by this seller, or when
    /// `new_name` collides with an existing channel (the table-wide UNIQUE
    /// COLLATE NOCASE constraint). Parameterized SQL only.
    pub fn rename_channel(
        &self,
        seller_id: i64,
        old_name: &str,
        new_name: &str,
        is_global: bool,
    ) -> Result<(), String> {
        let _ = is_global;
        let old = old_name.trim();
        let new = new_name.trim();
        if old.is_empty() || new.is_empty() {
            return Err("channel must not be empty".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let updated = conn.execute(
            "UPDATE channels SET channel = ?1 WHERE channel = ?2 COLLATE NOCASE AND seller_id = ?3",
            params![new, old, seller_id],
        );
        match updated {
            Ok(0) => Err(format!(
                "channel '{old}' not found (or not yours to rename)"
            )),
            Ok(_) => Ok(()),
            Err(e) => {
                if e.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) {
                    Err(format!("channel '{new}' already exists"))
                } else {
                    Err(format!("failed to rename channel '{old}' to '{new}': {e}"))
                }
            }
        }
    }

    /// Remove `channel` scoped to `seller_id`: a seller can only remove their
    /// own row, 0 removes only global rows — a cross-scope attempt (or an
    /// unknown name) deletes nothing and returns an honest Err, mirroring
    /// `add_channel`'s Result so the console can surface the refusal.
    pub fn remove_channel(&self, channel: &str, seller_id: i64) -> Result<(), String> {
        let ch = channel.trim();
        if ch.is_empty() {
            return Err("channel must not be empty".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        // `channel = ?1` compares with the column's NOCASE collation.
        let deleted = conn
            .execute(
                "DELETE FROM channels WHERE channel = ?1 AND seller_id = ?2",
                params![ch, seller_id],
            )
            .map_err(|e| format!("failed to remove channel '{ch}': {e}"))?;
        if deleted > 0 {
            Ok(())
        } else {
            Err(format!("channel '{ch}' not found (or not yours to remove)"))
        }
    }

    /// Make `channel` THE default (global namespace only): every other row
    /// loses is_default, then the channel is flipped to default when it
    /// exists as a global row or inserted as a new global row when missing.
    /// A channel that exists only under a seller's namespace is refused with
    /// an honest error — the global default must not silently hijack a
    /// seller-owned row (and no upsert could: the UNIQUE row belongs to the
    /// seller). The refusal is checked BEFORE any flag is touched, so a
    /// refused call leaves the current default exactly as it was.
    pub fn set_default_channel(&self, channel: &str) -> Result<(), String> {
        let ch = channel.trim();
        if ch.is_empty() {
            return Err("channel must not be empty".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let seller_owned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM channels WHERE channel = ?1 AND seller_id != 0",
                params![ch],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if seller_owned > 0 {
            return Err(format!(
                "channel '{ch}' belongs to a seller and cannot become the global default"
            ));
        }
        // Clear every default first, so afterwards exactly one row — the
        // global one — carries the flag ("switches exactly one row").
        let _ = conn.execute("UPDATE channels SET is_default = 0", []);
        let flipped = conn.execute(
            "UPDATE channels SET is_default = 1 WHERE channel = ?1 AND seller_id = 0",
            params![ch],
        );
        match flipped {
            Ok(n) if n > 0 => Ok(()),
            Ok(_) => {
                let now = Utc::now().to_rfc3339();
                conn.execute(
                    "INSERT INTO channels(channel,seller_id,is_default,created_at) VALUES(?1,0,1,?2)",
                    params![ch, now],
                )
                .map(|_| ())
                .map_err(|e| format!("failed to set default channel '{ch}': {e}"))
            }
            Err(e) => Err(format!("failed to set default channel '{ch}': {e}")),
        }
    }

    /// Resolve the global default channel: the channels table first (the
    /// single is_default global row), then the legacy free-text `channel`
    /// config key so existing installs keep working untouched. `None` =
    /// nothing configured.
    pub fn default_channel(&self) -> Option<String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let from_table: Option<String> = conn
            .query_row(
                "SELECT channel FROM channels WHERE is_default = 1 AND seller_id = 0 ORDER BY id LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        drop(conn);
        from_table
            .filter(|s| !s.trim().is_empty())
            .or_else(|| self.get_config("channel").filter(|s| !s.trim().is_empty()))
    }

    /// List channels: `None` = every row (owner view); `Some(seller)` = the
    /// global namespace plus that seller's own rows, in insertion order.
    pub fn list_channels(&self, seller_scope: Option<i64>) -> Vec<ChannelRow> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let sql = match seller_scope {
            Some(_) => {
                "SELECT id, channel, title, seller_id, is_default, created_at FROM channels WHERE seller_id = 0 OR seller_id = ?1 ORDER BY id"
            }
            None => {
                "SELECT id, channel, title, seller_id, is_default, created_at FROM channels ORDER BY id"
            }
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = match seller_scope {
            Some(sid) => stmt.query_map(params![sid], Self::map_channel_row).ok(),
            None => stmt.query_map([], Self::map_channel_row).ok(),
        };
        rows.map(|it| it.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    // ---- Claim flow (v1.3.x: setup becomes skippable) ----

    /// Whether any owner-role web account exists. Claim mode is exactly "no
    /// owner account"; once one exists it never returns.
    pub fn owner_account_exists(&self) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM web_accounts WHERE role = 'owner')",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|v| v != 0)
        .unwrap_or(false)
    }

    /// The stored one-time claim code, if one is live. An emptied value
    /// (written by `ensure_claim_code` on clear) counts as cleared.
    pub fn claim_code(&self) -> Option<String> {
        self.get_config("claim_code")
            .filter(|c| !c.trim().is_empty())
    }

    /// Startup maintenance for the claim flow; the caller prints the returned
    /// code once, loudly, to the console (CSPRNG-secret-printed-once
    /// pattern). Exactly the v1.3.x contract:
    /// - owner account exists -> any stored code is stale: cleared (claim
    ///   mode can never return), return None;
    /// - no owner and setup NOT completed (`setup_done` unset, the same flag
    ///   the web gate reads) -> keep the existing code across restarts
    ///   (stable while unclaimed) or generate a fresh CSPRNG one, return it;
    /// - no owner but setup completed (the Skip path) -> no code is minted
    ///   or printed at startup; a code stored before the skip stays stored
    ///   untouched, so a claim surface may still honor it.
    pub fn ensure_claim_code(&self) -> Option<String> {
        if self.owner_account_exists() {
            if self.claim_code().is_some() {
                self.set_config("claim_code", "");
            }
            return None;
        }
        match self.claim_code() {
            Some(c) => Some(c),
            None => {
                let code = crate::util::gen_token(12);
                self.set_config("claim_code", &code);
                Some(code)
            }
        }
    }

    // ---- Subscriptions: extend / bulk revoke ----
    pub fn extend_subscription(&self, id: i64, days: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cur: Option<String> = conn
            .query_row(
                "SELECT expires_at FROM subscriptions WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten();
        let Some(cur) = cur else {
            return false;
        };
        let base = chrono::DateTime::parse_from_rfc3339(&cur)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());
        let now = Utc::now();
        let start = if base > now { base } else { now };
        let new_exp = start + chrono::Duration::days(days.clamp(1, 3650));
        conn.execute(
            "UPDATE subscriptions SET expires_at=?2 WHERE id=?1",
            params![id, new_exp.to_rfc3339()],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn bulk_revoke_subscriptions(&self, status: Option<&str>) -> usize {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let n = if let Some(st) = status.filter(|s| !s.trim().is_empty() && *s != "all") {
            conn.execute(
                "UPDATE subscriptions SET status='revoked' WHERE status=?1",
                params![st],
            )
        } else {
            conn.execute(
                "UPDATE subscriptions SET status='revoked' WHERE status!='revoked'",
                [],
            )
        };
        let _ = now;
        n.unwrap_or(0)
    }

    pub fn get_order(&self, order_id: &str) -> Option<(i64, i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT seller_id, buyer_user_id, invoice_id FROM orders WHERE order_id=?1",
            params![order_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    // ---- Services / slots / bookings ----
    pub fn create_service(
        &self,
        seller_id: i64,
        name: &str,
        price: &str,
        asset: &str,
        duration_minutes: i64,
        channel_id: Option<&str>,
    ) -> Result<i64, String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO services(seller_id,name,price,asset,duration_minutes,channel_id,active,created_at) VALUES(?1,?2,?3,?4,?5,?6,1,?7)",
            params![seller_id, name, price, asset, duration_minutes, channel_id, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_services(
        &self,
        seller_scope: Option<i64>,
    ) -> Vec<(i64, i64, String, String, String, i64, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT id,seller_id,name,price,asset,duration_minutes,channel_id FROM services WHERE seller_id=?1 ORDER BY id DESC".to_string(), vec![Box::new(sid)])
        } else {
            ("SELECT id,seller_id,name,price,asset,duration_minutes,channel_id FROM services ORDER BY id DESC".to_string(), vec![])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn find_service_id(&self, seller_id: i64, name: &str) -> Option<i64> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id FROM services WHERE seller_id=?1 AND name=?2",
            params![seller_id, name],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn create_slot(
        &self,
        service_id: i64,
        seller_id: i64,
        starts_at: &str,
        capacity: i64,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO slots(service_id,seller_id,starts_at,capacity,status,booked,created_at) VALUES(?1,?2,?3,?4,'available',0,?5)",
            params![service_id, seller_id, starts_at, capacity, now],
        );
        conn.last_insert_rowid()
    }

    pub fn list_slots(&self, service_id: i64) -> Vec<(i64, String, i64, i64, String)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare("SELECT id,starts_at,capacity,booked,status FROM slots WHERE service_id=?1 ORDER BY starts_at") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![service_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Atomic slot claim (D4, ARCHITECTURE-V1.4 §2): ONE `BEGIN IMMEDIATE`
    /// transaction re-checks the slot (exists, belongs to `service_id`,
    /// status 'available', starts in the future), holds it, and inserts the
    /// 'held' booking with
    /// `expires_at = min(now + BOOKING_HOLD_MINUTES, slot start)` as RFC3339.
    /// Returns the integer booking row id. `Ok(None)` when the slot is gone,
    /// held/booked (claim-race losers included), in the past, or not the
    /// named service's slot; `Err("slot time is invalid")` only when the
    /// stored starts_at cannot be parsed — slots are expected to be created
    /// with a validated time, the db refuses nonsense rather than guessing.
    /// The seller is attributed from the slot row (slots.seller_id).
    pub fn create_booking(
        &self,
        service_id: i64,
        slot_id: i64,
        user_id: i64,
    ) -> Result<Option<i64>, String> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now();
        let _ = conn.execute_batch("BEGIN IMMEDIATE");
        let slot: Option<(i64, i64, String)> = match conn
            .query_row(
                "SELECT service_id, seller_id, starts_at FROM slots WHERE id = ?1 AND service_id = ?2",
                params![slot_id, service_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
        {
            Ok(v) => v,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.to_string());
            }
        };
        let Some((_svc_id, seller_id, starts_at)) = slot else {
            // slot gone, or not this service's slot
            let _ = conn.execute_batch("ROLLBACK");
            return Ok(None);
        };
        let start = match DateTime::parse_from_rfc3339(&starts_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err("slot time is invalid".to_string());
            }
        };
        if start <= now {
            // past slots are never bookable
            let _ = conn.execute_batch("ROLLBACK");
            return Ok(None);
        }
        let expires = std::cmp::min(now + chrono::Duration::minutes(BOOKING_HOLD_MINUTES), start)
            .to_rfc3339();
        // The claim itself, guarded on status: 'available' is the only state a
        // hold may move off. BEGIN IMMEDIATE already serialized us against
        // other Db callers; the guard keeps that invariant local to the SQL.
        let claimed = match conn.execute(
            "UPDATE slots SET status='held', held_until=?2 WHERE id=?1 AND status='available'",
            params![slot_id, expires],
        ) {
            Ok(n) => n,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.to_string());
            }
        };
        if claimed == 0 {
            let _ = conn.execute_batch("ROLLBACK");
            return Ok(None);
        }
        let token = crate::util::gen_token(8);
        let inserted = conn.execute(
            "INSERT INTO bookings(booking_id,seller_id,service_id,slot_id,user_id,status,invoice_id,expires_at,created_at) VALUES(?1,?2,?3,?4,?5,'held',NULL,?6,?7)",
            params![
                token,
                seller_id,
                service_id,
                slot_id,
                user_id,
                expires,
                now.to_rfc3339()
            ],
        );
        match inserted {
            Ok(_) => {
                let _ = conn.execute_batch("COMMIT");
                Ok(Some(conn.last_insert_rowid()))
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e.to_string())
            }
        }
    }

    pub fn list_bookings(
        &self,
        seller_scope: Option<i64>,
    ) -> Vec<(String, i64, i64, i64, String, Option<String>)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(sid) =
            seller_scope
        {
            ("SELECT booking_id,seller_id,service_id,user_id,status,expires_at FROM bookings WHERE seller_id=?1 ORDER BY id DESC".to_string(), vec![Box::new(sid)])
        } else {
            ("SELECT booking_id,seller_id,service_id,user_id,status,expires_at FROM bookings ORDER BY id DESC".to_string(), vec![])
        };
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Console booking list (BOOKINGS_UX_SPEC §3.3, §10.1): JOINed service
    /// name + slot times so the page renders without N+1 lookups. Scope:
    /// `Some(seller)` → bookings of that seller's services only, `None` →
    /// every booking (admin/owner). `status` filters at the SQL level when
    /// given (filter-after-LIMIT would silently hide matching rows); `limit`
    /// caps the rows (the console passes 200). Ordered by slot start, newest
    /// first, booking id as the tiebreaker.
    pub fn list_bookings_filtered(
        &self,
        seller_scope: Option<i64>,
        status: Option<&str>,
        limit: i64,
    ) -> Vec<BookingListRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = String::from(
            "SELECT b.id, b.booking_id, b.user_id, COALESCE(sv.name,''), s.starts_at, s.ends_at, b.status, b.expires_at, sv.seller_id FROM bookings b LEFT JOIN slots s ON s.id = b.slot_id LEFT JOIN services sv ON sv.id = b.service_id",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(sid) = seller_scope {
            sql.push_str(" WHERE sv.seller_id = ?1");
            params_vec.push(Box::new(sid));
        }
        if let Some(st) = status {
            if seller_scope.is_some() {
                sql.push_str(" AND b.status = ?2");
            } else {
                sql.push_str(" WHERE b.status = ?1");
            }
            params_vec.push(Box::new(st.to_string()));
        }
        sql.push_str(&format!(
            " ORDER BY s.starts_at DESC, b.id DESC LIMIT ?{}",
            params_vec.len() + 1
        ));
        params_vec.push(Box::new(limit));
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(params_vec.iter().map(|b| b.as_ref())),
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, String>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<i64>>(8)?,
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Buyer-scoped list for `/mybookings` (D8): the caller's own bookings,
    /// newest first, same row shape as the console list.
    pub fn list_bookings_for_user(&self, user_id: i64, limit: i64) -> Vec<BookingListRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT b.id, b.booking_id, b.user_id, COALESCE(sv.name,''), s.starts_at, s.ends_at, b.status, b.expires_at, sv.seller_id FROM bookings b LEFT JOIN slots s ON s.id = b.slot_id LEFT JOIN services sv ON sv.id = b.service_id WHERE b.user_id = ?1 ORDER BY b.id DESC LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![user_id, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Console/buyer cancel (BOOKINGS_UX_SPEC §10.2): `held|confirmed →
    /// cancelled`, scoped — `Some(seller)` must match the booking's service
    /// seller, `None` (admin/owner) acts on any row. The slot is returned to
    /// 'available' ONLY when it has not started and the hold on it is this
    /// booking's own (a slot re-claimed by another buyer after an expiry is
    /// left untouched). One transaction; true iff the booking row moved.
    pub fn cancel_booking_scoped(&self, booking_id: i64, seller_scope: Option<i64>) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now();
        let _ = conn.execute_batch("BEGIN IMMEDIATE");
        let row = match conn.query_row(
            "SELECT b.status, b.slot_id, b.expires_at, sv.seller_id FROM bookings b LEFT JOIN services sv ON sv.id = b.service_id WHERE b.id = ?1",
            params![booking_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        ) {
            Ok(v) => v,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
                return false;
            }
        };
        let (status, slot_id, expires_at, seller_id) = row;
        if seller_scope.is_some() && seller_id != seller_scope {
            let _ = conn.execute_batch("ROLLBACK");
            return false;
        }
        if status != "held" && status != "confirmed" {
            // terminal (declined/cancelled) or unknown — nothing to cancel
            let _ = conn.execute_batch("ROLLBACK");
            return false;
        }
        if let Some(sid) = slot_id {
            let slot: Option<(String, String, Option<String>)> = match conn
                .query_row(
                    "SELECT starts_at, status, held_until FROM slots WHERE id = ?1",
                    params![sid],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Option<String>>(2)?,
                        ))
                    },
                )
                .optional()
            {
                Ok(v) => v,
                Err(_) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return false;
                }
            };
            if let Some((starts_at, slot_status, held_until)) = slot {
                // Unparseable time counts as started: never free what we
                // cannot prove is still in the future.
                let started = DateTime::parse_from_rfc3339(&starts_at)
                    .map(|dt| dt.with_timezone(&Utc) <= now)
                    .unwrap_or(true);
                // 'booked' can only be this confirmed booking's own slot
                // (nothing else can claim a held/booked slot); a 'held' slot is
                // ours only when its stamp matches the booking's expiry — both
                // were written together at claim time.
                let ours =
                    slot_status == "booked" || held_until.as_deref() == expires_at.as_deref();
                if !started && ours {
                    let freed = conn.execute(
                        "UPDATE slots SET status='available', held_until=NULL WHERE id=?1 AND (status='booked' OR held_until=?2)",
                        params![sid, expires_at],
                    );
                    if freed.is_err() {
                        let _ = conn.execute_batch("ROLLBACK");
                        return false;
                    }
                }
            }
        }
        let n = conn
            .execute(
                "UPDATE bookings SET status='cancelled' WHERE id=?1 AND status IN ('held','confirmed')",
                params![booking_id],
            )
            .unwrap_or(0);
        if n == 1 {
            let _ = conn.execute_batch("COMMIT");
            true
        } else {
            let _ = conn.execute_batch("ROLLBACK");
            false
        }
    }

    /// Seller confirm/decline (D3/D8): the `held|expired → confirmed|declined`
    /// transitions, scoped like every other booking write — `Some(seller)`
    /// must match the booking's service seller, `None` (admin/owner) acts on
    /// any row. Only "confirmed"/"declined" are accepted (allowlist in code).
    /// Confirm requires the slot to still be in the future and — for the D3
    /// resurrection path (expired hold) — the slot to be free: still
    /// 'available', or 'held' with THIS booking's own hold stamp; a slot
    /// re-claimed by another booking makes the confirm lose (false, nothing
    /// mutated). Decline frees the slot back to 'available' when the hold on
    /// it is this booking's own. One transaction; true iff the booking moved.
    pub fn set_booking_status(
        &self,
        booking_id: i64,
        seller_scope: Option<i64>,
        status: &str,
    ) -> bool {
        let confirm = match status {
            "confirmed" => true,
            "declined" => false,
            _ => return false, // allowlist: nothing else is a legal target
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now();
        let _ = conn.execute_batch("BEGIN IMMEDIATE");
        let row = match conn.query_row(
            "SELECT b.status, b.slot_id, b.expires_at, sv.seller_id FROM bookings b LEFT JOIN services sv ON sv.id = b.service_id WHERE b.id = ?1",
            params![booking_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            },
        ) {
            Ok(v) => v,
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
                return false;
            }
        };
        let (booking_status, slot_id, expires_at, seller_id) = row;
        if seller_scope.is_some() && seller_id != seller_scope {
            let _ = conn.execute_batch("ROLLBACK");
            return false;
        }
        if booking_status != "held" && booking_status != "expired" {
            // terminal states are closed; unknown statuses too
            let _ = conn.execute_batch("ROLLBACK");
            return false;
        }
        if confirm {
            if let Some(sid) = slot_id {
                let slot: Option<(String, String, Option<String>)> = match conn
                    .query_row(
                        "SELECT starts_at, status, held_until FROM slots WHERE id = ?1",
                        params![sid],
                        |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Option<String>>(2)?,
                            ))
                        },
                    )
                    .optional()
                {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return false;
                    }
                };
                let Some((starts_at, slot_status, held_until)) = slot else {
                    // slot vanished: a confirm without a time cannot be honored
                    let _ = conn.execute_batch("ROLLBACK");
                    return false;
                };
                // A confirm always needs a future slot (past = nothing to
                // deliver); unparseable stored times refuse the same way.
                let future = DateTime::parse_from_rfc3339(&starts_at)
                    .map(|dt| dt.with_timezone(&Utc) > now)
                    .unwrap_or(false);
                // Free for THIS booking: unclaimed, or still carrying this
                // booking's own hold stamp (live hold, or the stale hold of an
                // expired booking the sweeper has not freed). 'booked' and a
                // foreign hold stamp both mean another booking took it.
                let free =
                    slot_status == "available" || held_until.as_deref() == expires_at.as_deref();
                if !future || !free {
                    let _ = conn.execute_batch("ROLLBACK");
                    return false;
                }
                let claimed = conn.execute(
                    "UPDATE slots SET status='booked', held_until=NULL WHERE id=?1 AND (status='available' OR (status='held' AND held_until=?2))",
                    params![sid, expires_at],
                );
                match claimed {
                    Ok(1) => {}
                    _ => {
                        let _ = conn.execute_batch("ROLLBACK");
                        return false;
                    }
                }
            }
            // Legacy slot-less bookings confirm without slot gating.
        } else if let Some(sid) = slot_id {
            // Decline always frees the slot — but only when the hold on it is
            // this booking's own ('booked' is never ours on a decline path:
            // another booking may have re-claimed and confirmed it).
            let freed = conn.execute(
                "UPDATE slots SET status='available', held_until=NULL WHERE id=?1 AND status='held' AND held_until=?2",
                params![sid, expires_at],
            );
            if freed.is_err() {
                let _ = conn.execute_batch("ROLLBACK");
                return false;
            }
        }
        let target = if confirm { "confirmed" } else { "declined" };
        let n = conn
            .execute(
                "UPDATE bookings SET status=?2 WHERE id=?1 AND status IN ('held','expired')",
                params![booking_id, target],
            )
            .unwrap_or(0);
        if n == 1 {
            let _ = conn.execute_batch("COMMIT");
            true
        } else {
            let _ = conn.execute_batch("ROLLBACK");
            false
        }
    }

    /// Expire held bookings past their response window (D3/D5): frees the
    /// slots first, then transitions `held → expired` — both statements carry
    /// the same predicates as before — and returns the transitioned rows as
    /// (bookings.id, buyer user_id, service name, seller_id) so the sweeper can
    /// DM each buyer from the right bot (the seller's bot attribution). The
    /// status transition is the exactly-once guard for those DMs: the returned
    /// list is only returned when its size matches the rows the UPDATE actually
    /// moved (a mismatch would risk double-DMing, so nothing is returned
    /// instead).
    pub fn expire_held_bookings(&self) -> Vec<(i64, i64, String, i64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        // free slots — unchanged transition
        let _ = conn.execute(
            "UPDATE slots SET status='available', held_until=NULL WHERE status='held' AND held_until IS NOT NULL AND held_until < ?1",
            params![now],
        );
        // collect the expiring rows (same predicate as the transition below)
        let rows: Vec<(i64, i64, String, i64)> = {
            let mut stmt = match conn.prepare(
                "SELECT b.id, b.user_id, COALESCE(sv.name,''), b.seller_id FROM bookings b LEFT JOIN services sv ON sv.id = b.service_id WHERE b.status='held' AND b.expires_at IS NOT NULL AND b.expires_at < ?1",
            ) {
                Ok(s) => s,
                Err(_) => return vec![],
            };
            let mapped = stmt.query_map(params![now], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            });
            match mapped {
                Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
                Err(_) => return vec![],
            }
        };
        let n = conn
            .execute(
                "UPDATE bookings SET status='expired' WHERE status='held' AND expires_at IS NOT NULL AND expires_at < ?1",
                params![now],
            )
            .unwrap_or(0);
        if n == rows.len() { rows } else { Vec::new() }
    }
}

// ---------------------------------------------------------------------------
// Phase 1: promo codes + shopping cart (core)
// ---------------------------------------------------------------------------

pub type PromoCodeRow = (
    String,
    i64,
    String,
    f64,
    Option<String>,
    Option<i64>,
    i64,
    f64,
    String,
    Option<i64>,
    Option<String>,
    i64,
    String,
);

impl Db {
    #[allow(clippy::too_many_arguments)]
    pub fn create_promo_code(
        &self,
        seller_id: i64,
        code: &str,
        promo_type: &str,
        value: f64,
        currency: Option<&str>,
        max_uses: Option<i64>,
        min_purchase: f64,
        scope: &str,
        scope_id: Option<i64>,
        expires_at: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO promo_codes (code, seller_id, promo_type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires_at, active, created_at) VALUES (?1,?2,?3,?4,?5,?6,0,?7,?8,?9,?10,1,?11)",
            params![code, seller_id, promo_type, value, currency, max_uses, min_purchase, scope, scope_id, expires_at, now],
        )
        .map_err(|e| format!("create_promo_code: {e}"))?;
        Ok(())
    }

    pub fn get_promo_code(&self, seller_id: i64, code: &str) -> Option<PromoCodeRow> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT code, seller_id, promo_type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires_at, active, created_at FROM promo_codes WHERE seller_id=?1 AND code=?2",
            params![seller_id, code],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?)),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_promo_codes(&self, seller_id: i64) -> Vec<PromoCodeRow> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare("SELECT code, seller_id, promo_type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires_at, active, created_at FROM promo_codes WHERE seller_id=?1 ORDER BY created_at DESC") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(params![seller_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn list_all_promo_codes(&self) -> Vec<PromoCodeRow> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare("SELECT code, seller_id, promo_type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires_at, active, created_at FROM promo_codes ORDER BY created_at DESC") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                r.get(12)?,
            ))
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn revoke_promo_code_all(&self, code: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE promo_codes SET active=0 WHERE code=?1",
            params![code],
        )
        .unwrap_or(0)
            > 0
    }

    pub fn revoke_promo_code(&self, seller_id: i64, code: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE promo_codes SET active=0 WHERE seller_id=?1 AND code=?2",
            params![seller_id, code],
        )
        .unwrap_or(0)
            > 0
    }

    /// Atomically redeem a promo code: validates expiry/usage/min-purchase/scope/active,
    /// increments uses, records redemption. Returns discount amount (in currency units) or error.
    pub fn redeem_promo_code(
        &self,
        seller_id: i64,
        code: &str,
        buyer_id: i64,
        invoice_id: Option<&str>,
        purchase_amount: f64,
    ) -> Result<f64, String> {
        let mut conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let row: Option<(String, f64, Option<i64>, i64, f64, String, Option<i64>, Option<String>, i64)> = tx
            .query_row(
                "SELECT promo_type, value, max_uses, uses, min_purchase, scope, scope_id, expires_at, active FROM promo_codes WHERE seller_id=?1 AND code=?2",
                params![seller_id, code],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (promo_type, value, max_uses, uses, min_purchase, scope, scope_id, expires_at, active) =
            row.ok_or_else(|| "promo code not found".to_string())?;
        if active != 1 {
            return Err("promo code inactive".to_string());
        }
        if let Some(exp) = &expires_at
            && Utc::now() > exp.parse::<DateTime<Utc>>().unwrap_or(Utc::now())
        {
            return Err("promo code expired".to_string());
        }
        if let Some(cap) = max_uses
            && uses >= cap
        {
            return Err("promo code usage cap reached".to_string());
        }
        if purchase_amount < min_purchase {
            return Err("minimum purchase not met".to_string());
        }
        // scope check is left to caller; store scope for reference.
        let _ = (scope, scope_id);
        let discount = match promo_type.as_str() {
            "percent" => purchase_amount * (value / 100.0),
            "fixed" => value.min(purchase_amount),
            _ => return Err("unknown promo type".to_string()),
        };
        tx.execute(
            "UPDATE promo_codes SET uses = uses + 1 WHERE seller_id=?1 AND code=?2",
            params![seller_id, code],
        )
        .map_err(|e| e.to_string())?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO promo_redemptions (code, seller_id, buyer_id, invoice_id, discount_amount, created_at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![code, seller_id, buyer_id, invoice_id, discount, now],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(discount)
    }

    pub fn list_promo_redemptions(
        &self,
        seller_id: i64,
        code: Option<&str>,
    ) -> Vec<(i64, String, i64, Option<String>, f64, String)> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let sql = if code.is_some() {
            "SELECT id, code, buyer_id, invoice_id, discount_amount, created_at FROM promo_redemptions WHERE seller_id=?1 AND code=?2 ORDER BY created_at DESC"
        } else {
            "SELECT id, code, buyer_id, invoice_id, discount_amount, created_at FROM promo_redemptions WHERE seller_id=?1 ORDER BY created_at DESC"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(i64, String, i64, Option<String>, f64, String)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        };
        let rows = if let Some(c) = code {
            stmt.query_map(params![seller_id, c], mapper)
        } else {
            stmt.query_map(params![seller_id], mapper)
        };
        rows.ok().map(|r| r.flatten().collect()).unwrap_or_default()
    }

    // ---- cart ----
    pub fn cart_add_item(
        &self,
        buyer_id: i64,
        seller_id: i64,
        item_type: &str,
        item_id: i64,
        qty: i64,
    ) -> Result<(), String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO cart_items (buyer_id, seller_id, item_type, item_id, qty, added_at) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(buyer_id, seller_id, item_type, item_id) DO UPDATE SET qty = qty + ?5",
            params![buyer_id, seller_id, item_type, item_id, qty, now],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn cart_get_items(&self, buyer_id: i64, seller_id: i64) -> Vec<(i64, String, i64, i64)> {
        // (id, item_type, item_id, qty)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare("SELECT id, item_type, item_id, qty FROM cart_items WHERE buyer_id=?1 AND seller_id=?2 ORDER BY added_at ASC") {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(params![buyer_id, seller_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    pub fn cart_update_qty(
        &self,
        buyer_id: i64,
        seller_id: i64,
        item_type: &str,
        item_id: i64,
        qty: i64,
    ) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if qty <= 0 {
            return conn.execute("DELETE FROM cart_items WHERE buyer_id=?1 AND seller_id=?2 AND item_type=?3 AND item_id=?4", params![buyer_id, seller_id, item_type, item_id]).unwrap_or(0) > 0;
        }
        conn.execute("UPDATE cart_items SET qty=?5 WHERE buyer_id=?1 AND seller_id=?2 AND item_type=?3 AND item_id=?4", params![buyer_id, seller_id, item_type, item_id, qty]).unwrap_or(0) > 0
    }

    pub fn cart_remove_item(
        &self,
        buyer_id: i64,
        seller_id: i64,
        item_type: &str,
        item_id: i64,
    ) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("DELETE FROM cart_items WHERE buyer_id=?1 AND seller_id=?2 AND item_type=?3 AND item_id=?4", params![buyer_id, seller_id, item_type, item_id]).unwrap_or(0) > 0
    }

    pub fn cart_clear(&self, buyer_id: i64, seller_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "DELETE FROM cart_items WHERE buyer_id=?1 AND seller_id=?2",
            params![buyer_id, seller_id],
        );
    }

    pub fn cart_count(&self, buyer_id: i64, seller_id: i64) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT COALESCE(SUM(qty),0) FROM cart_items WHERE buyer_id=?1 AND seller_id=?2",
            params![buyer_id, seller_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn cart_clear_all(&self, buyer_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute(
            "DELETE FROM cart_items WHERE buyer_id=?1",
            params![buyer_id],
        );
    }

    pub fn cart_get_all_items(&self, buyer_id: i64) -> Vec<(i64, i64, String, i64, i64)> {
        // (seller_id, id, item_type, item_id, qty) for every cart row of buyer
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT seller_id, id, item_type, item_id, qty FROM cart_items WHERE buyer_id=?1 ORDER BY added_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(params![buyer_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .ok()
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
    }

    // ---- Phase 2: referrals ----
    pub fn create_referral_code(&self, seller_id: i64, user_id: i64) -> String {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(code) = conn.query_row(
            "SELECT code FROM referral_codes WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id],
            |r| r.get::<_, String>(0),
        ) {
            return code;
        }
        let code = format!("REF{}{}", seller_id, user_id);
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("INSERT OR IGNORE INTO referral_codes (seller_id, user_id, code, created_at) VALUES (?1,?2,?3,?4)", params![seller_id, user_id, code, now]);
        conn.query_row(
            "SELECT code FROM referral_codes WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id],
            |r| r.get(0),
        )
        .unwrap_or(code)
    }

    pub fn get_referrer_by_code(&self, seller_id: i64, code: &str) -> Option<i64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT user_id FROM referral_codes WHERE seller_id=?1 AND code=?2",
            params![seller_id, code],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn create_referral(
        &self,
        seller_id: i64,
        referrer_id: i64,
        referee_id: i64,
        code: &str,
    ) -> Result<(), String> {
        if referrer_id == referee_id {
            return Err("self-referral not allowed".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("INSERT INTO referrals (seller_id, referrer_id, referee_id, code, status, created_at) VALUES (?1,?2,?3,?4,'pending',?5)", params![seller_id, referrer_id, referee_id, code, Utc::now().to_rfc3339()])
            .map_err(|e| format!("create_referral: {e}"))?;
        Ok(())
    }

    pub fn mark_referral_rewarded(
        &self,
        seller_id: i64,
        referee_id: i64,
        reward_value: f64,
        reward_type: &str,
    ) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute("UPDATE referrals SET status='rewarded', reward_value=?3, reward_type=?4, rewarded_at=?5 WHERE seller_id=?1 AND referee_id=?2 AND status='pending'", params![seller_id, referee_id, reward_value, reward_type, now]).unwrap_or(0) > 0
    }

    pub fn list_referrals(
        &self,
        seller_id: i64,
        referrer_id: Option<i64>,
    ) -> Vec<(i64, i64, i64, String, String, f64)> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let sql = if referrer_id.is_some() {
            "SELECT id, referrer_id, referee_id, code, status, reward_value FROM referrals WHERE seller_id=?1 AND referrer_id=?2 ORDER BY created_at DESC"
        } else {
            "SELECT id, referrer_id, referee_id, code, status, reward_value FROM referrals WHERE seller_id=?1 ORDER BY created_at DESC"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64, String, String, f64)> {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        };
        let rows = if let Some(rid) = referrer_id {
            stmt.query_map(params![seller_id, rid], mapper)
        } else {
            stmt.query_map(params![seller_id], mapper)
        };
        rows.ok().map(|r| r.flatten().collect()).unwrap_or_default()
    }

    // ---- Phase 2: bonus/loyalty balance ----
    pub fn bonus_balance(&self, seller_id: i64, user_id: i64) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT balance_minor FROM bonus_balances WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn bonus_credit(
        &self,
        seller_id: i64,
        user_id: i64,
        amount_minor: i64,
        reason: &str,
    ) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("INSERT INTO bonus_balances (user_id, seller_id, balance_minor, updated_at) VALUES (?2,?1,0,?3) ON CONFLICT(user_id, seller_id) DO NOTHING", params![seller_id, user_id, now]);
        let _ = conn.execute("UPDATE bonus_balances SET balance_minor = balance_minor + ?3, updated_at=?4 WHERE seller_id=?1 AND user_id=?2", params![seller_id, user_id, amount_minor, now]);
        let _ = conn.execute("INSERT INTO bonus_ledger (user_id, seller_id, amount_minor, reason, created_at) VALUES (?2,?1,?3,?4,?5)", params![seller_id, user_id, amount_minor, reason, now]);
        // return updated balance
        conn.query_row(
            "SELECT balance_minor FROM bonus_balances WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub fn bonus_debit(
        &self,
        seller_id: i64,
        user_id: i64,
        amount_minor: i64,
        reason: &str,
    ) -> Result<i64, String> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let bal: i64 = conn
            .query_row(
                "SELECT balance_minor FROM bonus_balances WHERE seller_id=?1 AND user_id=?2",
                params![seller_id, user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if bal < amount_minor {
            return Err("insufficient bonus balance".to_string());
        }
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("UPDATE bonus_balances SET balance_minor = balance_minor - ?3, updated_at=?4 WHERE seller_id=?1 AND user_id=?2", params![seller_id, user_id, amount_minor, now]);
        let _ = conn.execute("INSERT INTO bonus_ledger (user_id, seller_id, amount_minor, reason, created_at) VALUES (?2,?1,?3,?4,?5)", params![seller_id, user_id, -amount_minor, reason, now]);
        Ok(bal - amount_minor)
    }

    // ---- Phase 3: CRM + support tickets ----
    pub fn upsert_customer(&self, seller_id: i64, user_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("INSERT INTO customers (user_id, seller_id, first_seen, last_seen) VALUES (?2,?1,?3,?3) ON CONFLICT(user_id, seller_id) DO UPDATE SET last_seen=?3", params![seller_id, user_id, now]);
    }

    pub fn get_customer(
        &self,
        seller_id: i64,
        user_id: i64,
    ) -> Option<(
        i64,
        i64,
        String,
        String,
        Option<String>,
        Option<String>,
        f64,
        i64,
        String,
    )> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("SELECT user_id, seller_id, first_seen, last_seen, tags, notes, total_spent, order_count, status FROM customers WHERE seller_id=?1 AND user_id=?2", params![seller_id, user_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional().ok().flatten()
    }

    pub fn list_customers(
        &self,
        seller_id: i64,
        tag: Option<&str>,
        status: Option<&str>,
    ) -> Vec<(i64, String, Option<String>, f64, i64, String)> {
        // (user_id, last_seen, tags, total_spent, order_count, status)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut sql = "SELECT user_id, last_seen, tags, total_spent, order_count, status FROM customers WHERE seller_id=?1".to_string();
        if tag.is_some() {
            sql.push_str(" AND (',' || tags || ',') LIKE ?2");
        }
        if status.is_some() {
            if tag.is_some() {
                sql.push_str(" AND status=?3");
            } else {
                sql.push_str(" AND status=?2");
            }
        }
        sql.push_str(" ORDER BY last_seen DESC");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let tagpat = tag.map(|t| format!("%,{t},%"));
        let mapper = |r: &rusqlite::Row| -> rusqlite::Result<(i64,String,Option<String>,f64,i64,String)> { Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)) };
        let rows = match (tagpat.as_deref(), status) {
            (Some(t), Some(st)) => stmt.query_map(params![seller_id, t, st], mapper),
            (Some(t), None) => stmt.query_map(params![seller_id, t], mapper),
            (None, Some(st)) => stmt.query_map(params![seller_id, st], mapper),
            (None, None) => stmt.query_map(params![seller_id], mapper),
        };
        rows.ok().map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn update_customer_notes(&self, seller_id: i64, user_id: i64, notes: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE customers SET notes=?3 WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id, notes],
        )
        .unwrap_or(0)
            > 0
    }

    pub fn update_customer_tags(&self, seller_id: i64, user_id: i64, tags: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE customers SET tags=?3 WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id, tags],
        )
        .unwrap_or(0)
            > 0
    }

    pub fn set_customer_status(&self, seller_id: i64, user_id: i64, status: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute(
            "UPDATE customers SET status=?3 WHERE seller_id=?1 AND user_id=?2",
            params![seller_id, user_id, status],
        )
        .unwrap_or(0)
            > 0
    }

    pub fn record_customer_purchase(&self, seller_id: i64, user_id: i64, amount: f64) {
        self.upsert_customer(seller_id, user_id);
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let _ = conn.execute("UPDATE customers SET total_spent = total_spent + ?3, order_count = order_count + 1, last_seen = ?4 WHERE seller_id=?1 AND user_id=?2", params![seller_id, user_id, amount, Utc::now().to_rfc3339()]);
    }

    // ---- Phase 3: support tickets ----
    pub fn create_ticket(&self, seller_id: i64, buyer_id: i64, subject: &str) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute("INSERT INTO tickets (seller_id, buyer_id, subject, status, created_at, updated_at) VALUES (?1,?2,?3,'open',?4,?4)", params![seller_id, buyer_id, subject, now]).unwrap_or(0);
        conn.last_insert_rowid()
    }

    pub fn get_ticket(&self, id: i64) -> Option<(i64, i64, i64, String, String, String, String)> {
        // (id, seller_id, buyer_id, subject, status, created_at, updated_at)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row("SELECT id, seller_id, buyer_id, subject, status, created_at, updated_at FROM tickets WHERE id=?1", params![id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional().ok().flatten()
    }

    pub fn list_tickets(
        &self,
        seller_id: Option<i64>,
        buyer_id: Option<i64>,
        status: Option<&str>,
    ) -> Vec<(i64, i64, i64, String, String, String)> {
        // (id, seller_id, buyer_id, subject, status, updated_at)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut sql =
            "SELECT id, seller_id, buyer_id, subject, status, updated_at FROM tickets WHERE 1=1"
                .to_string();
        if seller_id.is_some() {
            sql.push_str(" AND seller_id=?1");
        }
        if buyer_id.is_some() {
            sql.push_str(" AND buyer_id=?2");
        }
        if status.is_some() {
            sql.push_str(" AND status=?3");
        }
        sql.push_str(" ORDER BY updated_at DESC");
        // simplify: only handle common combos via sequential filters using a generic approach is complex; do seller+buyer+status with params
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let mapper =
            |r: &rusqlite::Row| -> rusqlite::Result<(i64, i64, i64, String, String, String)> {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            };
        // Build params dynamically is awkward with rusqlite; handle the common seller-only case and buyer-only case.
        let rows = if let (Some(s), None, None) = (seller_id, buyer_id, status) {
            stmt.query_map(params![s], mapper)
        } else if let (Some(s), Some(b), None) = (seller_id, buyer_id, status) {
            stmt.query_map(params![s, b], mapper)
        } else if let (Some(s), Some(b), Some(st)) = (seller_id, buyer_id, status) {
            stmt.query_map(params![s, b, st], mapper)
        } else if let (None, Some(b), _) = (seller_id, buyer_id, status) {
            // buyer-only: re-query simpler
            let mut s2 = conn.prepare("SELECT id, seller_id, buyer_id, subject, status, updated_at FROM tickets WHERE buyer_id=?1 ORDER BY updated_at DESC").unwrap();
            return s2
                .query_map(params![b], mapper)
                .ok()
                .map(|r| r.flatten().collect())
                .unwrap_or_default();
        } else {
            stmt.query_map(params![], mapper)
        };
        rows.ok().map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn add_ticket_message(
        &self,
        ticket_id: i64,
        sender_role: &str,
        sender_id: i64,
        text: &str,
    ) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute("INSERT INTO ticket_messages (ticket_id, sender_role, sender_id, text, created_at) VALUES (?1,?2,?3,?4,?5)", params![ticket_id, sender_role, sender_id, text, now]).unwrap_or(0);
        let _ = conn.execute(
            "UPDATE tickets SET updated_at=?2 WHERE id=?1",
            params![ticket_id, now],
        );
        conn.last_insert_rowid()
    }

    pub fn list_ticket_messages(&self, ticket_id: i64) -> Vec<(i64, String, i64, String, String)> {
        // (id, sender_role, sender_id, text, created_at)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare("SELECT id, sender_role, sender_id, text, created_at FROM ticket_messages WHERE ticket_id=?1 ORDER BY created_at ASC") { Ok(s)=>s, Err(_)=>return vec![] };
        stmt.query_map(params![ticket_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .ok()
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
    }

    pub fn set_ticket_status(&self, id: i64, status: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE tickets SET status=?2, updated_at=?3 WHERE id=?1",
            params![id, status, now],
        )
        .unwrap_or(0)
            > 0
    }

    // ---- Phase 4: recovery & engagement ----
    pub fn mark_cart_abandoned(&self, buyer_id: i64, seller_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("INSERT INTO cart_abandonment (buyer_id, seller_id, abandoned_at, reminded_count) VALUES (?1,?2,?3,0) ON CONFLICT(buyer_id, seller_id) DO UPDATE SET abandoned_at=?3, reminded_count=0, reminded_at=NULL, recovered_at=NULL", params![buyer_id, seller_id, now]);
    }

    pub fn mark_cart_reminded(&self, buyer_id: i64, seller_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute("UPDATE cart_abandonment SET reminded_count = reminded_count + 1, reminded_at=?3 WHERE buyer_id=?1 AND seller_id=?2", params![buyer_id, seller_id, now]);
    }

    pub fn mark_cart_recovered(&self, buyer_id: i64, seller_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "UPDATE cart_abandonment SET recovered_at=?3 WHERE buyer_id=?1 AND seller_id=?2",
            params![buyer_id, seller_id, now],
        );
    }

    pub fn list_abandoned_carts(&self, min_hours: i64) -> Vec<(i64, i64, String, i64)> {
        // (buyer_id, seller_id, abandoned_at, reminded_count)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let cutoff = (Utc::now() - chrono::Duration::hours(min_hours)).to_rfc3339();
        let mut stmt = match conn.prepare("SELECT buyer_id, seller_id, abandoned_at, reminded_count FROM cart_abandonment WHERE recovered_at IS NULL AND abandoned_at <= ?1 ORDER BY abandoned_at ASC") { Ok(s)=>s, Err(_)=>return vec![] };
        stmt.query_map(params![cutoff], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
    }

    pub fn set_reorder_schedule(
        &self,
        buyer_id: i64,
        seller_id: i64,
        product_id: i64,
        interval_days: i64,
    ) {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let next = (Utc::now() + chrono::Duration::days(interval_days)).to_rfc3339();
        let _ = conn.execute("INSERT INTO reorder_schedules (buyer_id, seller_id, product_id, interval_days, last_ordered_at, next_reminder_at, enabled) VALUES (?1,?2,?3,?4,?5,?6,1) ON CONFLICT(buyer_id, seller_id, product_id) DO UPDATE SET interval_days=?4, last_ordered_at=?5, next_reminder_at=?6, enabled=1", params![buyer_id, seller_id, product_id, interval_days, now, next]);
    }

    pub fn list_due_reorders(&self) -> Vec<(i64, i64, i64, i64)> {
        // (id, buyer_id, seller_id, product_id)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        let mut stmt = match conn.prepare("SELECT id, buyer_id, seller_id, product_id FROM reorder_schedules WHERE enabled=1 AND next_reminder_at IS NOT NULL AND next_reminder_at <= ?1") { Ok(s)=>s, Err(_)=>return vec![] };
        stmt.query_map(params![now], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
    }

    pub fn disable_reorder(&self, buyer_id: i64, seller_id: i64, product_id: i64) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("UPDATE reorder_schedules SET enabled=0 WHERE buyer_id=?1 AND seller_id=?2 AND product_id=?3", params![buyer_id, seller_id, product_id]).unwrap_or(0) > 0
    }

    pub fn create_review(
        &self,
        buyer_id: i64,
        seller_id: i64,
        product_id: i64,
        rating: i64,
        text: Option<&str>,
    ) -> Result<i64, String> {
        if !(1..=5).contains(&rating) {
            return Err("rating must be 1..=5".to_string());
        }
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute("INSERT INTO reviews (buyer_id, seller_id, product_id, rating, text, approved, created_at) VALUES (?1,?2,?3,?4,?5,0,?6)", params![buyer_id, seller_id, product_id, rating, text, now])
            .map_err(|e| format!("create_review: {e}"))?;
        Ok(conn.last_insert_rowid())
    }

    pub fn list_reviews(
        &self,
        seller_id: i64,
        product_id: i64,
        approved_only: bool,
    ) -> Vec<(i64, i64, i64, String)> {
        // (id, buyer_id, rating, text)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let sql = if approved_only {
            "SELECT id, buyer_id, rating, COALESCE(text,'') FROM reviews WHERE seller_id=?1 AND product_id=?2 AND approved=1 ORDER BY created_at DESC"
        } else {
            "SELECT id, buyer_id, rating, COALESCE(text,'') FROM reviews WHERE seller_id=?1 AND product_id=?2 ORDER BY created_at DESC"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        stmt.query_map(params![seller_id, product_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .ok()
        .map(|r| r.flatten().collect())
        .unwrap_or_default()
    }

    pub fn approve_review(&self, id: i64) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.execute("UPDATE reviews SET approved=1 WHERE id=?1", params![id])
            .unwrap_or(0)
            > 0
    }

    pub fn avg_rating(&self, seller_id: i64, product_id: i64) -> Option<f64> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT AVG(rating) FROM reviews WHERE seller_id=?1 AND product_id=?2 AND approved=1",
            params![seller_id, product_id],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
    }

    // ---- Phase 5: business analytics ----
    /// Analytics revenue per asset (paid/executed basis), the only valid
    /// money breakdown for this seller.
    pub fn analytics_revenue_by_asset(&self, seller_id: i64) -> Vec<(String, f64)> {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT asset, COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE seller_id=?1 AND status IN ('paid','executed') GROUP BY asset ORDER BY asset",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        match stmt.query_map(params![seller_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => vec![],
        }
    }

    /// Single-asset revenue only. Multiple assets produce `MixedAssets` —
    /// a summed float across BTC and USDT is not a money amount.
    pub fn analytics_revenue_total(&self, seller_id: i64) -> Result<f64, MixedAssets> {
        let rows = self.analytics_revenue_by_asset(seller_id);
        match rows.len() {
            0 => Ok(0.0),
            1 => Ok(rows[0].1),
            _ => Err(MixedAssets),
        }
    }

    pub fn analytics_invoice_counts(&self, seller_id: i64) -> (i64, i64) {
        // (total, paid)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM invoices WHERE seller_id=?1",
                params![seller_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let paid: i64 = conn.query_row("SELECT COUNT(*) FROM invoices WHERE seller_id=?1 AND status IN ('paid','executed')", params![seller_id], |r| r.get(0)).unwrap_or(0);
        (total, paid)
    }

    /// Average order value is meaningful only within one denomination.
    /// Multiple assets produce `MixedAssets`; callers show per-asset lines.
    pub fn analytics_aov(&self, seller_id: i64) -> Result<f64, MixedAssets> {
        let rows = self.analytics_revenue_by_asset(seller_id);
        if rows.len() > 1 {
            return Err(MixedAssets);
        }
        let (_, paid) = self.analytics_invoice_counts(seller_id);
        if paid == 0 {
            return Ok(0.0);
        }
        Ok(rows.first().map(|r| r.1).unwrap_or(0.0) / paid as f64)
    }

    pub fn analytics_top_products(&self, seller_id: i64, limit: i64) -> Vec<(i64, i64)> {
        // (product_id, count) for paid product invoices; product_id stored in plan_id column for products? Use product_grants join.
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut stmt = match conn.prepare("SELECT product_id, COUNT(*) FROM product_grants WHERE seller_id=?1 GROUP BY product_id ORDER BY COUNT(*) DESC LIMIT ?2") { Ok(s)=>s, Err(_)=>return vec![] };
        stmt.query_map(params![seller_id, limit], |r| Ok((r.get(0)?, r.get(1)?)))
            .ok()
            .map(|r| r.flatten().collect())
            .unwrap_or_default()
    }

    pub fn analytics_repeat_buyers(&self, seller_id: i64) -> (i64, i64) {
        // (total distinct buyers, repeat buyers with >1 paid invoice)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let total: i64 = conn.query_row("SELECT COUNT(DISTINCT user_id) FROM invoices WHERE seller_id=?1 AND status IN ('paid','executed')", params![seller_id], |r| r.get(0)).unwrap_or(0);
        let repeat: i64 = conn.query_row("SELECT COUNT(*) FROM (SELECT user_id FROM invoices WHERE seller_id=?1 AND status IN ('paid','executed') GROUP BY user_id HAVING COUNT(*) > 1)", params![seller_id], |r| r.get(0)).unwrap_or(0);
        (total, repeat)
    }

    pub fn analytics_abandoned_cart_rate(&self, seller_id: i64) -> (i64, i64) {
        // (abandoned carts, recovered carts)
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let abandoned: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM cart_abandonment WHERE seller_id=?1",
                params![seller_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let recovered: i64 = conn.query_row("SELECT COUNT(*) FROM cart_abandonment WHERE seller_id=?1 AND recovered_at IS NOT NULL", params![seller_id], |r| r.get(0)).unwrap_or(0);
        (abandoned, recovered)
    }

    pub fn analytics_customer_count(&self, seller_id: i64) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        conn.query_row(
            "SELECT COUNT(*) FROM customers WHERE seller_id=?1",
            params![seller_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    // ---- Phase 6: compliance (data export + deletion) ----
    pub fn export_buyer_data(&self, seller_id: i64, user_id: i64) -> String {
        let customer = self.get_customer(seller_id, user_id);
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        // subscriptions
        let mut subs = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT id, plan_id, channel, expires_at, created_at FROM subscriptions WHERE seller_id=?1 AND user_id=?2")
            && let Ok(rows) = stmt.query_map(params![seller_id, user_id], |r| Ok(serde_json::json!({"id": r.get::<_,i64>(0)?, "plan_id": r.get::<_,Option<String>>(1)?, "channel": r.get::<_,Option<String>>(2)?, "expires_at": r.get::<_,Option<String>>(3)?, "created_at": r.get::<_,Option<String>>(4)?}))) {
            for r in rows.flatten() { subs.push(r); }
        }
        // orders
        let mut orders = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT order_id, product_id, status, tracking, created_at FROM orders WHERE seller_id=?1 AND buyer_user_id=?2")
            && let Ok(rows) = stmt.query_map(params![seller_id, user_id], |r| Ok(serde_json::json!({"order_id": r.get::<_,String>(0)?, "product_id": r.get::<_,i64>(1)?, "status": r.get::<_,String>(2)?, "tracking": r.get::<_,Option<String>>(3)?, "created_at": r.get::<_,String>(4)?}))) {
            for r in rows.flatten() { orders.push(r); }
        }
        // codes redeemed (activation_codes have no buyer link; skip)
        // cart
        let mut cart = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT item_type, item_id, qty FROM cart_items WHERE seller_id=?1 AND buyer_id=?2")
            && let Ok(rows) = stmt.query_map(params![seller_id, user_id], |r| Ok(serde_json::json!({"item_type": r.get::<_,String>(0)?, "item_id": r.get::<_,i64>(1)?, "qty": r.get::<_,i64>(2)?}))) {
            for r in rows.flatten() { cart.push(r); }
        }
        // reviews
        let mut reviews = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT product_id, rating, text, created_at FROM reviews WHERE seller_id=?1 AND buyer_id=?2")
            && let Ok(rows) = stmt.query_map(params![seller_id, user_id], |r| Ok(serde_json::json!({"product_id": r.get::<_,i64>(0)?, "rating": r.get::<_,i64>(1)?, "text": r.get::<_,Option<String>>(2)?, "created_at": r.get::<_,String>(3)?}))) {
            for r in rows.flatten() { reviews.push(r); }
        }
        // bonus balance
        let balance: i64 = conn
            .query_row(
                "SELECT balance_minor FROM bonus_balances WHERE seller_id=?1 AND user_id=?2",
                params![seller_id, user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        // customer
        let out = serde_json::json!({
            "seller_id": seller_id,
            "user_id": user_id,
            "customer": customer.map(|c| serde_json::json!({"first_seen": c.2, "last_seen": c.3, "tags": c.4, "notes": c.5, "total_spent": c.6, "order_count": c.7, "status": c.8})),
            "subscriptions": subs,
            "orders": orders,
            "cart": cart,
            "reviews": reviews,
            "bonus_balance_minor": balance,
        });
        serde_json::to_string_pretty(&out).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn delete_buyer_data(&self, seller_id: i64, user_id: i64) -> i64 {
        let conn = self.conn.lock().unwrap_or_else(|p| p.into_inner());
        let mut total = 0i64;
        let stmts = [
            "DELETE FROM subscriptions WHERE seller_id=?1 AND user_id=?2",
            "DELETE FROM product_grants WHERE seller_id=?1 AND user_id=?2",
            "DELETE FROM orders WHERE seller_id=?1 AND buyer_user_id=?2",
            "DELETE FROM cart_items WHERE seller_id=?1 AND buyer_id=?2",
            "DELETE FROM reviews WHERE seller_id=?1 AND buyer_id=?2",
            "DELETE FROM referrals WHERE seller_id=?1 AND (referrer_id=?2 OR referee_id=?2)",
            "DELETE FROM bonus_balances WHERE seller_id=?1 AND user_id=?2",
            "DELETE FROM bonus_ledger WHERE seller_id=?1 AND user_id=?2",
            "DELETE FROM tickets WHERE seller_id=?1 AND buyer_id=?2",
            "DELETE FROM customers WHERE seller_id=?1 AND user_id=?2",
        ];
        for sql in stmts {
            total += conn.execute(sql, params![seller_id, user_id]).unwrap_or(0) as i64;
        }
        // ticket messages cascade
        let _ = conn.execute("DELETE FROM ticket_messages WHERE ticket_id IN (SELECT id FROM tickets WHERE seller_id=?1 AND buyer_id=?2)", params![seller_id, user_id]);
        total
    }
}
// end phase6 (placeholder marker)

/// Cross-cutting moderation storage. Seller-scoped methods take `seller_id`
/// in their WHERE clause (or INSERT values): a seller can never read, change
/// or remove another seller's rows. Per-bot overrides additionally require
/// `bot_id`, so the same bot id under a different seller is a different
/// namespace. The `moderation_global_floor_*` methods are the exception: the
/// floor is global (no seller_id) and owner-only at the web layer — storage
/// just exposes explicit methods that any seller-scoped caller cannot reach
/// implicitly. All SQL is parameterized and lives only in this layer.
impl Db {
    fn global_floor_term_from_row(
        r: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ModerationGlobalFloorTerm> {
        Ok(ModerationGlobalFloorTerm {
            id: r.get(0)?,
            term: r.get(1)?,
            mode: r.get(2)?,
            created_at: r.get(3)?,
            updated_at: r.get(4)?,
        })
    }

    /// Query the persistent global floor. `Err` here means the table is
    /// unavailable/unreadable — callers fall back to the compile-time defaults
    /// and mark the resolved policy as degraded.
    fn query_global_floor(&self) -> Result<Vec<ModerationGlobalFloorTerm>, rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = conn.prepare(
            "SELECT id,term,mode,created_at,updated_at FROM moderation_global_floor ORDER BY term",
        )?;
        let rows = stmt.query_map([], Self::global_floor_term_from_row)?;
        rows.collect()
    }

    /// List the owner-enforced global floor terms, sorted by term. The table
    /// is seeded empty, so a fresh/legacy install returns an empty vector.
    pub fn list_moderation_global_floor(&self) -> Vec<ModerationGlobalFloorTerm> {
        self.query_global_floor().unwrap_or_default()
    }

    /// Add (or re-mode) one owner-enforced global floor term. The (term) UNIQUE
    /// key makes this idempotent; calling with a stricter/looser mode updates
    /// it in place. Invalid terms/modes are refused (fail closed). The web
    /// layer MUST gate this to the owner — storage has no implicit auth.
    pub fn add_moderation_global_floor_term(&self, term: &str, mode: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        if !is_moderation_mode(mode) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_global_floor(term,mode,created_at,updated_at) VALUES(?1,?2,?3,?3)
             ON CONFLICT(term) DO UPDATE SET mode=excluded.mode, updated_at=excluded.updated_at",
            params![term, mode, now],
        )
        .is_ok()
    }

    /// Remove one owner-enforced global floor term. Seller/allow-list callers
    /// never reach this method; once removed, the term is no longer protected
    /// and normal seller precedence applies.
    pub fn remove_moderation_global_floor_term(&self, term: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM moderation_global_floor WHERE term=?1",
            params![term],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    /// Floor terms in resolver input form. Returns `None` when the table is
    /// unreadable so `resolve_effective_policy` can use the fallback default.
    fn load_global_floor_terms(&self) -> Option<Vec<ModerationTermInput>> {
        self.query_global_floor().ok().map(|rows| {
            rows.into_iter()
                .map(|r| ModerationTermInput {
                    term: r.term,
                    mode: r.mode,
                })
                .collect()
        })
    }

    fn blocked_word_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ModerationBlockedWord> {
        Ok(ModerationBlockedWord {
            id: r.get(0)?,
            seller_id: r.get(1)?,
            term: r.get(2)?,
            mode: r.get(3)?,
            enabled: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    }

    fn allow_word_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ModerationAllowWord> {
        Ok(ModerationAllowWord {
            id: r.get(0)?,
            seller_id: r.get(1)?,
            term: r.get(2)?,
            created_at: r.get(3)?,
        })
    }

    fn bot_override_term_from_row(
        r: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ModerationBotOverrideTerm> {
        Ok(ModerationBotOverrideTerm {
            id: r.get(0)?,
            seller_id: r.get(1)?,
            bot_id: r.get(2)?,
            term: r.get(3)?,
            mode: r.get(4)?,
            created_at: r.get(5)?,
            updated_at: r.get(6)?,
        })
    }

    fn appeal_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ModerationAppeal> {
        Ok(ModerationAppeal {
            id: r.get(0)?,
            seller_id: r.get(1)?,
            bot_id: r.get(2)?,
            decision_id: r.get(3)?,
            message_ref: r.get(4)?,
            reason: r.get(5)?,
            status: r.get(6)?,
            created_at: r.get(7)?,
            updated_at: r.get(8)?,
        })
    }

    fn decision_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ModerationDecision> {
        Ok(ModerationDecision {
            id: r.get(0)?,
            seller_id: r.get(1)?,
            bot_id: r.get(2)?,
            term: r.get(3)?,
            action: r.get(4)?,
            reason: r.get(5)?,
            message_ref: r.get(6)?,
            created_at: r.get(7)?,
        })
    }

    /// Upsert the single inheritance-sentinel row (term='', mode=NULL) for a
    /// (seller, bot) scope. It is the source of truth for "Using your shop's
    /// rules" (true) vs "Custom rules for this bot" (false).
    fn upsert_bot_override_sentinel(
        conn: &Connection,
        seller_id: i64,
        bot_id: i64,
        inherits: bool,
    ) -> bool {
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_bot_overrides(seller_id,bot_id,term,mode,inherits,created_at,updated_at)
             VALUES(?1,?2,'',NULL,?3,?4,?4)
             ON CONFLICT(seller_id,bot_id,term) DO UPDATE SET inherits=excluded.inherits, updated_at=excluded.updated_at",
            params![seller_id, bot_id, inherits as i64, now],
        )
        .is_ok()
    }

    // ---- (b) Seller-wide blocked words ----

    /// Add or update one seller-scoped blocked word. Invalid terms/modes are
    /// refused (fail closed); the (seller_id, term) UNIQUE key makes this
    /// idempotent so the console's "Add a word" can be retried safely.
    pub fn add_blocked_word(&self, seller_id: i64, term: &str, mode: &str, enabled: bool) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        if !is_moderation_mode(mode) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_blocked_words(seller_id,term,mode,enabled,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?5)
             ON CONFLICT(seller_id,term) DO UPDATE SET mode=excluded.mode, enabled=excluded.enabled, updated_at=excluded.updated_at",
            params![seller_id, term, mode, enabled as i64, now],
        )
        .is_ok()
    }

    pub fn set_blocked_word_mode(&self, seller_id: i64, term: &str, mode: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        if !is_moderation_mode(mode) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE moderation_blocked_words SET mode=?3, updated_at=?4 WHERE seller_id=?1 AND term=?2",
            params![seller_id, term, mode, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn set_blocked_word_enabled(&self, seller_id: i64, term: &str, enabled: bool) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE moderation_blocked_words SET enabled=?3, updated_at=?4 WHERE seller_id=?1 AND term=?2",
            params![seller_id, term, enabled as i64, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn remove_blocked_word(&self, seller_id: i64, term: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM moderation_blocked_words WHERE seller_id=?1 AND term=?2",
            params![seller_id, term],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn get_blocked_word(&self, seller_id: i64, term: &str) -> Option<ModerationBlockedWord> {
        let term = normalize_moderation_term(term)?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,term,mode,enabled,created_at,updated_at
             FROM moderation_blocked_words WHERE seller_id=?1 AND term=?2",
            params![seller_id, term],
            Self::blocked_word_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_blocked_words(&self, seller_id: i64) -> Vec<ModerationBlockedWord> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,term,mode,enabled,created_at,updated_at
             FROM moderation_blocked_words WHERE seller_id=?1 ORDER BY term",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id], Self::blocked_word_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    // ---- (b) "Always allow" list ----

    pub fn add_allow_word(&self, seller_id: i64, term: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_allow_words(seller_id,term,created_at) VALUES(?1,?2,?3)
             ON CONFLICT(seller_id,term) DO NOTHING",
            params![seller_id, term, now],
        )
        .is_ok()
    }

    pub fn remove_allow_word(&self, seller_id: i64, term: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM moderation_allow_words WHERE seller_id=?1 AND term=?2",
            params![seller_id, term],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn get_allow_word(&self, seller_id: i64, term: &str) -> Option<ModerationAllowWord> {
        let term = normalize_moderation_term(term)?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,term,created_at FROM moderation_allow_words WHERE seller_id=?1 AND term=?2",
            params![seller_id, term],
            Self::allow_word_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_allow_words(&self, seller_id: i64) -> Vec<ModerationAllowWord> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,term,created_at FROM moderation_allow_words WHERE seller_id=?1 ORDER BY term",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id], Self::allow_word_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    // ---- (c) Per-bot override (scoped by BOTH seller_id and bot_id) ----

    /// Set "Using your shop's rules" (true) / "Custom rules for this bot"
    /// (false). Custom terms are preserved through the toggle so returning
    /// to inheritance is non-destructive.
    pub fn set_bot_override_inherits(&self, seller_id: i64, bot_id: i64, inherits: bool) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::upsert_bot_override_sentinel(&conn, seller_id, bot_id, inherits)
    }

    /// Whether a bot inherits its seller's rules. Defaults to true when no
    /// sentinel row exists — "inherits by default" per the spec.
    pub fn bot_override_inherits(&self, seller_id: i64, bot_id: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT inherits FROM moderation_bot_overrides WHERE seller_id=?1 AND bot_id=?2 AND term=''",
            params![seller_id, bot_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(|v| v != 0)
        .unwrap_or(true)
    }

    /// Add/update a custom term for one bot. Adding the first custom term
    /// activates "Custom rules for this bot" (inherits=false) so the console
    /// need not issue a second call; the sentinel is written first so a
    /// failed term insert can never leave an ignored term behind.
    pub fn add_bot_override_term(
        &self,
        seller_id: i64,
        bot_id: i64,
        term: &str,
        mode: &str,
    ) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        if !is_moderation_mode(mode) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !Self::upsert_bot_override_sentinel(&conn, seller_id, bot_id, false) {
            return false;
        }
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_bot_overrides(seller_id,bot_id,term,mode,inherits,created_at,updated_at)
             VALUES(?1,?2,?3,?4,0,?5,?5)
             ON CONFLICT(seller_id,bot_id,term) DO UPDATE SET mode=excluded.mode, inherits=0, updated_at=excluded.updated_at",
            params![seller_id, bot_id, term, mode, now],
        )
        .is_ok()
    }

    pub fn remove_bot_override_term(&self, seller_id: i64, bot_id: i64, term: &str) -> bool {
        let Some(term) = normalize_moderation_term(term) else {
            return false;
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM moderation_bot_overrides WHERE seller_id=?1 AND bot_id=?2 AND term=?3 AND term<>''",
            params![seller_id, bot_id, term],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn get_bot_override_term(
        &self,
        seller_id: i64,
        bot_id: i64,
        term: &str,
    ) -> Option<ModerationBotOverrideTerm> {
        let term = normalize_moderation_term(term)?;
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,bot_id,term,mode,created_at,updated_at
             FROM moderation_bot_overrides
             WHERE seller_id=?1 AND bot_id=?2 AND term=?3 AND term<>''",
            params![seller_id, bot_id, term],
            Self::bot_override_term_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    pub fn list_bot_override_terms(
        &self,
        seller_id: i64,
        bot_id: i64,
    ) -> Vec<ModerationBotOverrideTerm> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,bot_id,term,mode,created_at,updated_at
             FROM moderation_bot_overrides
             WHERE seller_id=?1 AND bot_id=?2 AND term<>'' ORDER BY term",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id, bot_id], Self::bot_override_term_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    // ---- Appeals review queue (opaque refs only, never message content) ----

    /// Create an appeal. `message_ref` must be an opaque token supplied by the
    /// bot-side flow (server); `reason` is the short appeal justification, capped
    /// by [`MODERATION_MAX_REASON_LEN`] — neither may carry the filtered body.
    ///
    /// Atomic dedup (security LOW): the unique index on
    /// `(seller_id, COALESCE(bot_id, 0), message_ref)` makes concurrent
    /// double-submissions a no-op instead of two rows. `None` is returned on
    /// conflict — the existing `Option<i64>` signature already means "no row
    /// created", so server/web call sites need no change (the handler's pre-check
    /// still reports `AlreadyRequested` in the non-racy case).
    pub fn create_moderation_appeal(
        &self,
        seller_id: i64,
        bot_id: Option<i64>,
        decision_id: Option<i64>,
        message_ref: &str,
        reason: Option<&str>,
    ) -> Option<i64> {
        let message_ref = normalize_moderation_ref(message_ref)?;
        let reason = trim_moderation_reason(reason);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let inserted = conn
            .execute(
                "INSERT INTO moderation_appeals(seller_id,bot_id,decision_id,message_ref,reason,status,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,'pending',?6,?6)
                 ON CONFLICT(seller_id, COALESCE(bot_id, 0), message_ref) DO NOTHING",
                params![seller_id, bot_id, decision_id, message_ref, reason, now],
            )
            .ok()?;
        // `last_insert_rowid()` is per-connection and is NOT advanced by a
        // DO NOTHING conflict; only report an id when a row was really added.
        if inserted == 0 {
            return None;
        }
        Some(conn.last_insert_rowid())
    }

    pub fn get_moderation_appeal(
        &self,
        seller_id: i64,
        appeal_id: i64,
    ) -> Option<ModerationAppeal> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT id,seller_id,bot_id,decision_id,message_ref,reason,status,created_at,updated_at
             FROM moderation_appeals WHERE seller_id=?1 AND id=?2",
            params![seller_id, appeal_id],
            Self::appeal_from_row,
        )
        .optional()
        .ok()
        .flatten()
    }

    /// `status` is an optional filter ('pending'|'approved'|'upheld'); an
    /// invalid filter yields no rows instead of a raw SQL fragment.
    pub fn list_moderation_appeals(
        &self,
        seller_id: i64,
        status: Option<&str>,
    ) -> Vec<ModerationAppeal> {
        let status = match status {
            None => None,
            Some(s) if is_moderation_appeal_status(s) => Some(s.to_owned()),
            Some(_) => return Vec::new(),
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,bot_id,decision_id,message_ref,reason,status,created_at,updated_at
             FROM moderation_appeals
             WHERE seller_id=?1 AND (?2 IS NULL OR status=?2) ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id, status], Self::appeal_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Bot-narrowed variant: both seller_id and bot_id are bound, so the same
    /// bot id under another seller is a disjoint queue.
    pub fn list_moderation_appeals_for_bot(
        &self,
        seller_id: i64,
        bot_id: i64,
        status: Option<&str>,
    ) -> Vec<ModerationAppeal> {
        let status = match status {
            None => None,
            Some(s) if is_moderation_appeal_status(s) => Some(s.to_owned()),
            Some(_) => return Vec::new(),
        };
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,bot_id,decision_id,message_ref,reason,status,created_at,updated_at
             FROM moderation_appeals
             WHERE seller_id=?1 AND bot_id=?2 AND (?3 IS NULL OR status=?3) ORDER BY id DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id, bot_id, status], Self::appeal_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn set_moderation_appeal_status(
        &self,
        seller_id: i64,
        appeal_id: i64,
        status: &str,
    ) -> bool {
        if !is_moderation_appeal_status(status) {
            return false;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE moderation_appeals SET status=?3, updated_at=?4 WHERE seller_id=?1 AND id=?2",
            params![seller_id, appeal_id, status, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    // ---- Append-only decision audit ----

    /// Record one moderation decision. There is intentionally NO message-text
    /// parameter: `term` is the matched policy term and `message_ref` is an
    /// opaque caller token. `reason` is a short code/justification, capped.
    pub fn log_moderation_decision(
        &self,
        seller_id: i64,
        bot_id: Option<i64>,
        term: Option<&str>,
        action: &str,
        reason: Option<&str>,
        message_ref: Option<&str>,
    ) -> Option<i64> {
        let term = term.and_then(normalize_moderation_term);
        let action = normalize_moderation_ref(action)?;
        let reason = trim_moderation_reason(reason);
        let message_ref = message_ref.and_then(normalize_moderation_ref);
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO moderation_decisions(seller_id,bot_id,term,action,reason,message_ref,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)",
            params![seller_id, bot_id, term, action, reason, message_ref, now],
        )
        .ok()?;
        Some(conn.last_insert_rowid())
    }

    pub fn list_moderation_decisions(&self, seller_id: i64, limit: i64) -> Vec<ModerationDecision> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,bot_id,term,action,reason,message_ref,created_at
             FROM moderation_decisions WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![seller_id, limit.max(0)], Self::decision_from_row)
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    pub fn list_moderation_decisions_for_bot(
        &self,
        seller_id: i64,
        bot_id: i64,
        limit: i64,
    ) -> Vec<ModerationDecision> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,bot_id,term,action,reason,message_ref,created_at
             FROM moderation_decisions WHERE seller_id=?1 AND bot_id=?2 ORDER BY id DESC LIMIT ?3",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(
            params![seller_id, bot_id, limit.max(0)],
            Self::decision_from_row,
        )
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    // ---- Resolution ----

    /// Resolve the effective policy for one (seller, optional bot) scope.
    /// Reads the persistent owner floor, then applies seller rules, per-bot
    /// custom rules (only when the override is active), and the seller
    /// allow-list — see [`apply_moderation_precedence`] for the exact ladder.
    /// If moderation storage is not ready or the floor cannot be read, the
    /// compile-time defaults are used and `enforcement_degraded` is set so
    /// callers fail closed instead of trusting a partial policy.
    pub fn resolve_effective_policy(
        &self,
        seller_id: i64,
        bot_id: Option<i64>,
    ) -> EffectiveModerationPolicy {
        let seller_terms: Vec<ModerationTermInput> = self
            .list_blocked_words(seller_id)
            .into_iter()
            .filter(|w| w.enabled)
            .map(|w| ModerationTermInput {
                term: w.term,
                mode: w.mode,
            })
            .collect();
        let allow_terms: Vec<String> = self
            .list_allow_words(seller_id)
            .into_iter()
            .map(|w| w.term)
            .collect();

        let mut bot_override_active = false;
        let mut bot_terms: Vec<ModerationTermInput> = Vec::new();
        if let Some(bot_id) = bot_id {
            bot_override_active = !self.bot_override_inherits(seller_id, bot_id);
            if bot_override_active {
                bot_terms = self
                    .list_bot_override_terms(seller_id, bot_id)
                    .into_iter()
                    .map(|w| ModerationTermInput {
                        term: w.term,
                        mode: w.mode,
                    })
                    .collect();
            }
        }

        // Persistent floor is the shipped source of truth. Fall back to the
        // compile-time defaults only when the table is unavailable/unreadable.
        let storage_ready = self.moderation_storage_ready.load(Ordering::Relaxed);
        let (floor_terms, floor_read_failed) = match self.load_global_floor_terms() {
            Some(terms) => (terms, false),
            None => (
                MODERATION_FLOOR_TERMS
                    .iter()
                    .map(|t| ModerationTermInput {
                        term: (*t).to_owned(),
                        mode: MODERATION_FLOOR_MODE.to_owned(),
                    })
                    .collect(),
                true,
            ),
        };
        let terms = apply_moderation_precedence(
            MODERATION_FLOOR_MODE,
            &floor_terms,
            &seller_terms,
            &bot_terms,
            &allow_terms,
        );
        EffectiveModerationPolicy {
            seller_id,
            bot_id,
            floor_mode: MODERATION_FLOOR_MODE.to_owned(),
            terms,
            bot_override_active,
            enforcement_degraded: !storage_ready || floor_read_failed,
        }
    }
}

// ---- Capability verticals: catalogue search, file shares, automation ----

/// A stored file share. Field order: id, seller_id, bot_id, file_id, name,
/// description, token, active. The file_id is a Telegram reference, not bytes.
pub type FileShareRow = (i64, i64, i64, String, String, String, String, i64);
/// A keyword auto-reply rule. Field order: id, seller_id, bot_id, keyword,
/// response, active.
pub type AutomationRuleRow = (i64, i64, i64, String, String, i64);

impl Db {
    /// Substring search over active, publicly sellable products. The legacy
    /// seller 0 namespace is excluded; the caller decides what to render.
    pub fn search_products(&self, query: &str, limit: i64) -> Vec<ProductRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pattern = format!("%{}%", query.trim());
        let mut stmt = match conn.prepare(
            "SELECT id,seller_id,name,price,asset,kind,delivery,stock,active FROM products \
             WHERE active=1 AND seller_id<>0 AND (name LIKE ?1 OR kind LIKE ?1) \
             ORDER BY id LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![pattern, limit], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get(8)?,
                ))
            })
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn create_file_share(
        &self,
        seller_id: i64,
        bot_id: i64,
        file_id: &str,
        name: &str,
        description: &str,
        token: &str,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO file_shares(seller_id,bot_id,file_id,name,description,token,active,created_at) \
             VALUES(?1,?2,?3,?4,?5,?6,1,?7)",
            params![seller_id, bot_id, file_id, name, description, token, now],
        );
        conn.last_insert_rowid()
    }

    fn file_share_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileShareRow> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
            r.get(7)?,
        ))
    }

    const FILE_SHARE_COLUMNS: &str = "id,seller_id,bot_id,file_id,name,description,token,active";

    pub fn get_file_share(&self, id: i64) -> Option<FileShareRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sql = format!(
            "SELECT {} FROM file_shares WHERE id=?1",
            Self::FILE_SHARE_COLUMNS
        );
        conn.query_row(&sql, params![id], Self::file_share_from_row)
            .optional()
            .ok()
            .flatten()
    }

    pub fn get_file_share_by_token(&self, token: &str) -> Option<FileShareRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sql = format!(
            "SELECT {} FROM file_shares WHERE token=?1",
            Self::FILE_SHARE_COLUMNS
        );
        conn.query_row(&sql, params![token], Self::file_share_from_row)
            .optional()
            .ok()
            .flatten()
    }

    /// Owner/admin view: shares created by one seller on one bot, or all.
    pub fn list_file_shares(
        &self,
        seller_scope: Option<i64>,
        bot_id: Option<i64>,
    ) -> Vec<FileShareRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = format!(
            "SELECT {} FROM file_shares WHERE active=1",
            Self::FILE_SHARE_COLUMNS
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(sid) = seller_scope {
            values.push(Box::new(sid));
            sql.push_str(&format!(" AND seller_id=?{}", values.len()));
        }
        if let Some(bid) = bot_id {
            values.push(Box::new(bid));
            sql.push_str(&format!(" AND bot_id=?{}", values.len()));
        }
        sql.push_str(" ORDER BY id DESC");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(values.iter().map(|b| b.as_ref())),
                Self::file_share_from_row,
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    /// Buyer view: only shares explicitly granted to this user.
    pub fn list_file_shares_for_user(&self, user_id: i64) -> Vec<FileShareRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sql = format!(
            "SELECT {0} FROM file_shares f JOIN file_share_grants g ON g.share_id=f.id \
             WHERE g.user_id=?1 AND f.active=1 ORDER BY f.id DESC",
            Self::FILE_SHARE_COLUMNS
                .split(',')
                .map(|c| format!("f.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(params![user_id], Self::file_share_from_row)
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn grant_file_share(&self, share_id: i64, user_id: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT OR IGNORE INTO file_share_grants(share_id,user_id,created_at) VALUES(?1,?2,?3)",
            params![share_id, user_id, now],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn revoke_file_share(&self, share_id: i64, user_id: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "DELETE FROM file_share_grants WHERE share_id=?1 AND user_id=?2",
            params![share_id, user_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn file_share_grant_exists(&self, share_id: i64, user_id: i64) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT 1 FROM file_share_grants WHERE share_id=?1 AND user_id=?2",
            params![share_id, user_id],
            |_| Ok(()),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
    }

    pub fn set_file_share_active(&self, id: i64, seller_id: i64, active: bool) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE file_shares SET active=?1 WHERE id=?2 AND seller_id=?3",
            params![i64::from(active), id, seller_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub fn create_automation_rule(
        &self,
        seller_id: i64,
        bot_id: i64,
        keyword: &str,
        response: &str,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        let _ = conn.execute(
            "INSERT INTO automation_rules(seller_id,bot_id,keyword,response,active,created_at) \
             VALUES(?1,?2,?3,?4,1,?5)",
            params![seller_id, bot_id, keyword, response, now],
        );
        conn.last_insert_rowid()
    }

    fn automation_rule_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<AutomationRuleRow> {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
        ))
    }

    /// Rules visible to one seller on one bot. `None` scopes mean all rows
    /// (admin view); `active_only` is used by the trigger runner.
    pub fn list_automation_rules(
        &self,
        seller_scope: Option<i64>,
        bot_id: Option<i64>,
        active_only: bool,
    ) -> Vec<AutomationRuleRow> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut sql = String::from(
            "SELECT id,seller_id,bot_id,keyword,response,active FROM automation_rules WHERE 1=1",
        );
        let mut values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(sid) = seller_scope {
            values.push(Box::new(sid));
            sql.push_str(&format!(" AND seller_id=?{}", values.len()));
        }
        if let Some(bid) = bot_id {
            values.push(Box::new(bid));
            sql.push_str(&format!(" AND bot_id=?{}", values.len()));
        }
        if active_only {
            sql.push_str(" AND active=1");
        }
        sql.push_str(" ORDER BY id");
        let mut stmt = match conn.prepare(&sql) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map(
                rusqlite::params_from_iter(values.iter().map(|b| b.as_ref())),
                Self::automation_rule_from_row,
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
    }

    pub fn set_automation_rule_active(&self, id: i64, seller_id: i64, active: bool) -> bool {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.execute(
            "UPDATE automation_rules SET active=?1 WHERE id=?2 AND seller_id=?3",
            params![i64::from(active), id, seller_id],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod delete_bot_tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn delete_bot_removes_row_and_nulls_selected_bot_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delete_bot_unit.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let bot_id = db.create_bot("bot-key-del", 100, None, "");
        assert!(bot_id > 0);

        // Unknown id returns false and changes nothing.
        assert!(!db.delete_bot(999_999));

        db.create_web_account("alice", "pw", "admin", None)
            .await
            .unwrap();
        let (acc_id, ..) = db.get_web_account_by_username("alice").unwrap();
        let token = db.create_web_session(acc_id, "admin", None, 7);
        let hash = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(token.as_bytes());
            let bytes = h.finalize();
            let mut s = String::with_capacity(64);
            for b in bytes.iter() {
                s.push_str(&format!("{:02x}", b));
            }
            s
        };
        assert!(db.set_web_session_selected_bot(&hash, Some(bot_id)));
        assert_eq!(db.get_web_session_selected_bot(&hash), Some(bot_id));

        // Deleting removes the bot row...
        assert!(db.delete_bot(bot_id));
        assert!(db.get_bot(bot_id).is_none());
        // ...and clears any session selection pointing at it.
        assert_eq!(db.get_web_session_selected_bot(&hash), None);

        // A second delete reports no row.
        assert!(!db.delete_bot(bot_id));
        let _ = fs::remove_file(path);
    }
}
