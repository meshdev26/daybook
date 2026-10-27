#![allow(clippy::type_complexity)]
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Mutex;

type PostRow = (i64, i64, String, i32, String, Option<String>, String);
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

/// Booking row for the console table and `/mybookings` (L7/L8), adjusted to
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
/// point for the whole booking state machine; pub so the bot copy (L8) and
/// console copy (L7) quote the same number instead of a hardcoded string.
pub const BOOKING_HOLD_MINUTES: i64 = 60;
/// TTL in minutes for one-time console setup links (D7): the DM'd link dies
/// unclaimed after this long.
pub const SETUP_LINK_TTL_MINUTES: i64 = 15;
/// Purpose tag one-time setup links carry in `web_pending_mfa.purpose` (D7);
/// pub because the claim routes (L7) validate the same string on GET/POST.
pub const SETUP_LINK_PURPOSE: &str = "setup_link";
// Named-field row (not a tuple alias) — a new public surface consumed by the
// web channels manager; seller_id 0 = global namespace, is_default flags the
// single global default.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRow {
    pub id: i64,
    pub channel: String,
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
pub struct Db {
    conn: Mutex<Connection>,
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
        };
        db.init_schema()?;
        let _ = db.migrate();
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

    pub fn get_config(&self, key: &str) -> Option<String> {
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
    pub fn log_post(
        &self,
        seller_id: i64,
        chat_id: &str,
        message_id: i32,
        kind: &str,
        content: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO posts(seller_id,chat_id,message_id,kind,content,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![seller_id, chat_id, message_id, kind, content, now],
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
            ("SELECT id,seller_id,chat_id,message_id,kind,content,created_at FROM posts WHERE seller_id=?1 ORDER BY id DESC LIMIT ?2".to_string(),
             vec![Box::new(sid), Box::new(limit)])
        } else {
            ("SELECT id,seller_id,chat_id,message_id,kind,content,created_at FROM posts ORDER BY id DESC LIMIT ?1".to_string(),
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
                    ))
                },
            )
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
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
    pub fn is_admin(&self, user_id: i64, owner_id: i64) -> bool {
        if user_id == owner_id {
            return true;
        }
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        conn.query_row(
            "SELECT 1 FROM admins WHERE user_id = ?1",
            params![user_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .is_some()
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

    pub fn get_web_session(&self, token: &str) -> Option<(i64, String, Option<i64>)> {
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
            "SELECT account_id, role, seller_id FROM web_sessions WHERE token_hash=?1 AND expires_at > ?2",
            params![hash, now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional().ok().flatten()
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
    pub fn create_refund(
        &self,
        seller_id: i64,
        invoice_id: Option<&str>,
        user_id: Option<i64>,
        amount: &str,
        asset: &str,
        reason: Option<&str>,
    ) -> i64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = Utc::now().to_rfc3339();
        // best-effort: fall back to the invoice's buyer when user_id unknown
        let user_id = match user_id {
            Some(u) => Some(u),
            None => invoice_id.and_then(|inv| {
                conn.query_row(
                    "SELECT user_id FROM invoices WHERE invoice_id=?1",
                    params![inv],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            }),
        };
        let _ = conn.execute(
            "INSERT INTO refunds(seller_id,invoice_id,user_id,amount,asset,reason,status,created_at,updated_at) VALUES(?1,?2,?3,?4,?5,?6,'pending',?7,?7)",
            params![seller_id, invoice_id, user_id, amount, asset, reason, now],
        );
        conn.last_insert_rowid()
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
            "UPDATE refunds SET status='executing', updated_at=?2 WHERE id=?1 AND status='approved'",
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
            "UPDATE payouts SET status='executing', updated_at=?2 WHERE id=?1 AND status='approved'",
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
    pub fn revenue_total(&self, seller_scope: Option<i64>) -> f64 {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let sql = if seller_scope.is_some() {
            "SELECT COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE seller_id=?1 AND status='paid'"
        } else {
            "SELECT COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE status='paid'"
        };
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return 0.0,
        };
        let res = if let Some(sid) = seller_scope {
            stmt.query_row(params![sid], |r| r.get::<_, f64>(0))
        } else {
            stmt.query_row([], |r| r.get::<_, f64>(0))
        };
        res.unwrap_or(0.0)
    }

    pub fn revenue_by_seller(&self) -> Vec<(i64, f64)> {
        let mut out = Vec::new();
        for sid in self.list_sellers() {
            out.push((sid, self.revenue_total(Some(sid))));
        }
        out
    }

    /// Per-asset paid revenue — the L5 ruling (CRYPTO_PAY.md, ruling 5):
    /// summing CAST(amount AS REAL) across assets (0.5 BTC + 50 USDT) is a
    /// meaningless number and a display defect, so revenue is reported per
    /// asset and must never be totalled cross-asset downstream. Same
    /// paid-invoice basis as revenue_total: `WHERE status='paid'`.
    pub fn revenue_by_asset(&self) -> Vec<(String, f64)> {
        let conn = self
            .conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT asset, COALESCE(SUM(CAST(amount AS REAL)),0) FROM invoices WHERE status='paid' GROUP BY asset ORDER BY asset",
        ) {
            Ok(s) => s,
            Err(_) => return vec![],
        };
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))
            .ok();
        match rows {
            Some(iter) => iter.filter_map(|r| r.ok()).collect(),
            None => vec![],
        }
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

    // ---- Append-only additions for the L8 API/observability task
    // (2026-10-25). Kept as a clearly-marked block at the end of the impl so
    // parallel edits elsewhere in db.rs cannot collide; no existing lines
    // were modified. Lock handling follows the poisoned-mutex recovery
    // pattern used by the mcp_* methods above.

    /// Cheap liveness probe for `/health`: `SELECT 1`. Returns false on any
    /// error so the health endpoint can report `db: "error"` honestly
    /// instead of panicking. (L8 addition)
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
    /// (L8 addition)
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
    /// cross-seller attempt deletes nothing at all. (L8 addition)
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
            seller_id: r.get(2)?,
            is_default: r.get::<_, i64>(3)? != 0,
            created_at: r.get(4)?,
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
                "SELECT id, channel, seller_id, is_default, created_at FROM channels WHERE seller_id = 0 OR seller_id = ?1 ORDER BY id"
            }
            None => {
                "SELECT id, channel, seller_id, is_default, created_at FROM channels ORDER BY id"
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
    /// (bookings.id, buyer user_id, service name) so the sweeper can DM each
    /// buyer. The status transition is the exactly-once guard for those DMs:
    /// the returned list is only returned when its size matches the rows the
    /// UPDATE actually moved (a mismatch would risk double-DMing, so nothing
    /// is returned instead).
    pub fn expire_held_bookings(&self) -> Vec<(i64, i64, String)> {
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
        let rows: Vec<(i64, i64, String)> = {
            let mut stmt = match conn.prepare(
                "SELECT b.id, b.user_id, COALESCE(sv.name,'') FROM bookings b LEFT JOIN services sv ON sv.id = b.service_id WHERE b.status='held' AND b.expires_at IS NOT NULL AND b.expires_at < ?1",
            ) {
                Ok(s) => s,
                Err(_) => return vec![],
            };
            let mapped = stmt.query_map(params![now], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    #[test]
    fn db_basic() {
        let path = "/tmp/opencode/test_agent2.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        assert_eq!(db.seller_channel(1).as_deref(), Some("@chan"));
        db.add_plan(1, "P", "1", "USDT", 7, Some("@chan"));
        let plans = db.list_plans(Some(1));
        assert_eq!(plans.len(), 1);
        let pid = db.get_plan(1, "P").unwrap().0;
        let code = db.create_code(1, Some(pid), Some(7), 1, None, 12);
        assert_eq!(code.len(), 12);
        let (_id, sid, p, _vd, _mu, uses, _exp) = db.get_code(&code).unwrap();
        assert_eq!(sid, 1);
        assert_eq!(p, Some(pid));
        assert_eq!(uses, 0);
        let res = db.redeem_code(&code, 42);
        assert!(res.is_ok());
        let code2 = db.get_code(&code).unwrap();
        assert_eq!(code2.5, 1);
    }

    // ---- v1.4 bookings / setup links / MCP tokens / per-asset revenue ----

    fn booking_db(tag: &str) -> (Db, String) {
        let path = format!("/tmp/opencode/test_bookings_{tag}.db");
        let _ = fs::remove_file(&path);
        let db = Db::new(&path).unwrap();
        (db, path)
    }

    fn service_with_slot(db: &Db, seller: i64, name: &str, starts_at: &str) -> (i64, i64) {
        let svc = db
            .create_service(seller, name, "10", "USDT", 60, None)
            .expect("service created");
        let slot = db.create_slot(svc, seller, starts_at, 1);
        (svc, slot)
    }

    fn rfc_in_minutes(minutes: i64) -> String {
        (Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
    }

    /// Point a booking and its slot's hold stamp into the past — exactly the
    /// values claim time wrote together — so the next expire_held_bookings()
    /// pass transitions the booking and frees the slot like real clock time
    /// would (tests must not wait out BOOKING_HOLD_MINUTES).
    fn force_expiry(path: &str, booking_id: i64) {
        let past = (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339();
        let conn = Connection::open(path).unwrap();
        conn.execute(
            "UPDATE bookings SET expires_at=?1 WHERE id=?2",
            params![past, booking_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE slots SET held_until=(SELECT expires_at FROM bookings WHERE id=?1) WHERE id=(SELECT slot_id FROM bookings WHERE id=?1)",
            params![booking_id],
        )
        .unwrap();
    }

    #[test]
    fn test_booking_claim_is_atomic() {
        let (db, _path) = booking_db("claim");
        let (svc, slot) = service_with_slot(&db, 11, "consult", &rfc_in_minutes(120));
        let mut won = Vec::new();
        for u in 1..=8i64 {
            if let Ok(Some(id)) = db.create_booking(svc, slot, 100 + u) {
                won.push(id);
            }
        }
        assert_eq!(won.len(), 1, "exactly one buyer wins the slot");

        let slots = db.list_slots(svc);
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].4, "held");
        assert_eq!(db.list_bookings(Some(11)).len(), 1);
    }

    #[test]
    fn test_booking_claim_rejects_missing_past_and_nonsense() {
        let (db, _path) = booking_db("reject");
        let (svc, slot) = service_with_slot(&db, 11, "consult", &rfc_in_minutes(60));
        // unknown slot id
        assert_eq!(db.create_booking(svc, 999_999, 1).unwrap(), None);
        // past slot
        let (svc_past, slot_past) = service_with_slot(&db, 11, "past", &rfc_in_minutes(-60));
        assert_eq!(db.create_booking(svc_past, slot_past, 1).unwrap(), None);
        // unparseable stored time — the db refuses nonsense
        let (svc_bad, slot_bad) = service_with_slot(&db, 11, "bad", "not-a-timestamp");
        assert_eq!(
            db.create_booking(svc_bad, slot_bad, 1).unwrap_err(),
            "slot time is invalid"
        );
        // service/slot pair mismatch: the slot belongs to svc, not svc_past
        assert_eq!(db.create_booking(svc_past, slot, 1).unwrap(), None);
        // nothing mutated by any rejected claim
        assert_eq!(db.list_bookings(Some(11)).len(), 0);
        assert_eq!(db.list_slots(svc)[0].4, "available");
    }

    #[test]
    fn test_booking_expires_at_is_min_of_hold_and_start() {
        let (db, _path) = booking_db("expiry_window");
        // slot sooner than the hold window: expires_at == slot start (D3)
        let (svc, slot) = service_with_slot(&db, 11, "soon", &rfc_in_minutes(10));
        assert!(db.create_booking(svc, slot, 1).unwrap().is_some());
        let row = &db.list_bookings_for_user(1, 10)[0];
        let exp = DateTime::parse_from_rfc3339(row.7.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let start = DateTime::parse_from_rfc3339(&rfc_in_minutes(10))
            .unwrap()
            .with_timezone(&Utc);
        assert!((exp - start).num_seconds().abs() < 5);
        // slot later than the hold window: expires_at ≈ now + BOOKING_HOLD_MINUTES
        let (svc2, slot2) = service_with_slot(&db, 11, "far", &rfc_in_minutes(240));
        assert!(db.create_booking(svc2, slot2, 2).unwrap().is_some());
        let row2 = &db.list_bookings_for_user(2, 10)[0];
        let exp2 = DateTime::parse_from_rfc3339(row2.7.as_deref().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let expected = Utc::now() + chrono::Duration::minutes(BOOKING_HOLD_MINUTES);
        assert!((exp2 - expected).num_seconds().abs() < 10);
    }

    #[test]
    fn test_list_bookings_filtered_scope_status_limit() {
        let (db, _path) = booking_db("filtered");
        let (svc1, slot1) = service_with_slot(&db, 11, "alpha", &rfc_in_minutes(60));
        let (svc2, slot2) = service_with_slot(&db, 22, "beta", &rfc_in_minutes(120));
        let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
        let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
        assert!(db.set_booking_status(b2, Some(22), "confirmed"));

        // scope: seller 11 sees only their own rows
        let s1 = db.list_bookings_filtered(Some(11), None, 100);
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].0, b1);
        assert_eq!(s1[0].1.len(), 8, "public booking token present");
        assert_eq!(s1[0].2, 100);
        assert_eq!(s1[0].3, "alpha");
        assert!(s1[0].4.is_some(), "slot starts_at joined");
        assert_eq!(s1[0].6, "held");
        assert!(s1[0].7.is_some(), "expires_at joined");
        assert_eq!(s1[0].8, Some(11));

        // status filter at SQL level
        let held = db.list_bookings_filtered(None, Some("held"), 100);
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].0, b1);
        let confirmed = db.list_bookings_filtered(Some(22), Some("confirmed"), 100);
        assert_eq!(confirmed.len(), 1);
        assert_eq!(confirmed[0].0, b2);
        assert_eq!(
            db.list_bookings_filtered(None, Some("cancelled"), 100)
                .len(),
            0
        );

        // limit caps the rows; ordering is slot start DESC (beta starts later)
        assert_eq!(db.list_bookings_filtered(None, None, 1).len(), 1);
        let all = db.list_bookings_filtered(None, None, 100);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, b2);
        assert_eq!(all[1].0, b1);
    }

    #[test]
    fn test_list_bookings_for_user() {
        let (db, _path) = booking_db("for_user");
        let (svc1, slot1) = service_with_slot(&db, 11, "alpha", &rfc_in_minutes(60));
        let (svc2, slot2) = service_with_slot(&db, 22, "beta", &rfc_in_minutes(90));
        let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
        let b2 = db.create_booking(svc2, slot2, 100).unwrap().unwrap();
        let rows = db.list_bookings_for_user(100, 10);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, b2, "newest first");
        assert_eq!(rows[1].0, b1);
        assert!(rows.iter().all(|r| r.2 == 100));
        assert_eq!(db.list_bookings_for_user(999, 10).len(), 0);
        assert_eq!(db.list_bookings_for_user(100, 1).len(), 1);
    }

    #[test]
    fn test_cancel_booking_scoped_rules() {
        let (db, path) = booking_db("cancel");
        // held + future slot: the owning seller cancels, slot freed
        let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(60));
        let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
        assert!(
            !db.cancel_booking_scoped(b1, Some(99)),
            "wrong seller scope"
        );
        assert!(db.cancel_booking_scoped(b1, Some(11)));
        assert_eq!(db.list_slots(svc1)[0].4, "available");
        assert_eq!(db.list_bookings(Some(11))[0].4, "cancelled");
        assert!(!db.cancel_booking_scoped(b1, Some(11)), "terminal now");

        // admin (None) may cancel another seller's booking
        let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(60));
        let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
        assert!(db.cancel_booking_scoped(b2, None));
        assert_eq!(db.list_slots(svc2)[0].4, "available");

        // confirmed booking on a future slot: cancel frees the 'booked' slot
        let (svc3, slot3) = service_with_slot(&db, 33, "c", &rfc_in_minutes(60));
        let b3 = db.create_booking(svc3, slot3, 300).unwrap().unwrap();
        assert!(db.set_booking_status(b3, Some(33), "confirmed"));
        assert_eq!(db.list_slots(svc3)[0].4, "booked");
        assert!(db.cancel_booking_scoped(b3, Some(33)));
        assert_eq!(db.list_slots(svc3)[0].4, "available");

        // confirmed booking whose slot already started: cancelled, but the
        // slot is NOT freed back to available
        let (svc4, slot4) = service_with_slot(&db, 44, "d", &rfc_in_minutes(60));
        let b4 = db.create_booking(svc4, slot4, 400).unwrap().unwrap();
        assert!(db.set_booking_status(b4, Some(44), "confirmed"));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE slots SET starts_at=?1 WHERE id=?2",
                params![rfc_in_minutes(-30), slot4],
            )
            .unwrap();
        }
        assert!(db.cancel_booking_scoped(b4, Some(44)));
        assert_eq!(db.list_slots(svc4)[0].4, "booked", "started slot stays");
    }

    #[test]
    fn test_set_booking_status_confirm_decline_and_resurrection() {
        let (db, path) = booking_db("status");
        // held → confirmed books the future slot
        let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(60));
        let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
        assert!(
            !db.set_booking_status(b1, Some(99), "confirmed"),
            "wrong seller"
        );
        assert!(!db.set_booking_status(b1, Some(11), "pending"), "allowlist");
        assert!(db.set_booking_status(b1, Some(11), "confirmed"));
        assert_eq!(db.list_slots(svc1)[0].4, "booked");
        assert_eq!(db.list_bookings(Some(11))[0].4, "confirmed");
        assert!(!db.set_booking_status(b1, Some(11), "declined"), "terminal");
        assert!(
            !db.set_booking_status(b1, Some(11), "confirmed"),
            "terminal"
        );

        // confirm refuses a past slot; decline still frees it
        let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(60));
        let b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE slots SET starts_at=?1 WHERE id=?2",
                params![rfc_in_minutes(-5), slot2],
            )
            .unwrap();
        }
        assert!(!db.set_booking_status(b2, Some(22), "confirmed"));
        assert_eq!(db.list_bookings(Some(22))[0].4, "held", "unchanged");
        assert_eq!(db.list_slots(svc2)[0].4, "held", "slot untouched");
        assert!(db.set_booking_status(b2, Some(22), "declined"));
        assert_eq!(db.list_bookings(Some(22))[0].4, "declined");
        assert_eq!(db.list_slots(svc2)[0].4, "available");

        // D3 resurrection: expired hold whose slot was freed → confirm re-books
        let (svc3, slot3) = service_with_slot(&db, 33, "c", &rfc_in_minutes(120));
        let b3 = db.create_booking(svc3, slot3, 300).unwrap().unwrap();
        force_expiry(&path, b3);
        assert_eq!(db.expire_held_bookings().len(), 1);
        assert_eq!(db.list_slots(svc3)[0].4, "available");
        assert!(db.set_booking_status(b3, Some(33), "confirmed"));
        assert_eq!(db.list_slots(svc3)[0].4, "booked");
        assert_eq!(db.list_bookings(Some(33))[0].4, "confirmed");

        // resurrection loses when another booking re-claimed the slot
        let (svc4, slot4) = service_with_slot(&db, 44, "d", &rfc_in_minutes(120));
        let b4 = db.create_booking(svc4, slot4, 400).unwrap().unwrap();
        force_expiry(&path, b4);
        db.expire_held_bookings();
        assert!(db.create_booking(svc4, slot4, 401).unwrap().is_some());
        assert!(
            !db.set_booking_status(b4, Some(44), "confirmed"),
            "slot taken"
        );
        let rows = db.list_bookings_filtered(Some(44), None, 100);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows.iter().find(|r| r.2 == 400).unwrap().6,
            "expired",
            "loser untouched"
        );
        assert_eq!(
            rows.iter().find(|r| r.2 == 401).unwrap().6,
            "held",
            "winner intact"
        );
    }

    #[test]
    fn test_expire_held_bookings_returns_each_row_once() {
        let (db, path) = booking_db("sweep");
        let (svc1, slot1) = service_with_slot(&db, 11, "a", &rfc_in_minutes(120));
        let (svc2, slot2) = service_with_slot(&db, 22, "b", &rfc_in_minutes(240));
        let b1 = db.create_booking(svc1, slot1, 100).unwrap().unwrap();
        let _b2 = db.create_booking(svc2, slot2, 200).unwrap().unwrap();
        force_expiry(&path, b1);
        let rows = db.expire_held_bookings();
        assert_eq!(rows, vec![(b1, 100, "a".to_string())]);
        // exactly-once: the second pass returns nothing
        assert!(db.expire_held_bookings().is_empty());
        // the untouched booking keeps its hold; b1's slot was freed
        assert_eq!(db.list_bookings(Some(22))[0].4, "held");
        assert_eq!(db.list_slots(svc2)[0].4, "held");
        assert_eq!(db.list_slots(svc1)[0].4, "available");
    }

    #[test]
    fn test_setup_link_single_use_purpose_and_ttl() {
        let (db, path) = booking_db("setup");
        let raw = db.create_setup_link(42);
        assert_eq!(raw.len(), 48);
        // single use: exactly one consume wins
        assert_eq!(db.consume_pending_token(&raw, SETUP_LINK_PURPOSE), Some(42));
        assert_eq!(db.consume_pending_token(&raw, SETUP_LINK_PURPOSE), None);
        assert_eq!(db.consume_pending_token("nope", SETUP_LINK_PURPOSE), None);
        // purpose separation: an MFA verify token is not a setup link
        let verify_raw = crate::util::gen_token(48);
        let expires = (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
        db.create_pending_mfa(
            sha256_hex(&verify_raw),
            42,
            "verify",
            None::<String>,
            expires,
        )
        .unwrap();
        assert_eq!(
            db.consume_pending_token(&verify_raw, SETUP_LINK_PURPOSE),
            None
        );
        assert_eq!(db.consume_pending_token(&verify_raw, "verify"), Some(42));
        // TTL: an expired setup link dies with the same generic None
        let raw2 = db.create_setup_link(43);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "UPDATE web_pending_mfa SET expires_at=?1 WHERE account_id=43",
                params![(Utc::now() - chrono::Duration::minutes(1)).to_rfc3339()],
            )
            .unwrap();
        }
        assert_eq!(db.consume_pending_token(&raw2, SETUP_LINK_PURPOSE), None);
    }

    #[test]
    fn test_mcp_token_lifecycle_and_masking() {
        let (db, path) = booking_db("mcp");
        let (id, raw) = db.create_mcp_token(7, Some("ci"));
        assert_eq!(raw.len(), 48);
        // the hash-lookup path mcp::lookup_mcp_token already consumes
        let row = db
            .get_mcp_token_by_hash(&sha256_hex(&raw))
            .expect("hash finds the token");
        assert_eq!(row.0, id);
        assert_eq!(row.1, 7);
        assert_eq!(row.2.as_deref(), Some("ci"));
        assert_eq!(row.3, 0, "not revoked");

        // masked list: prefix + name only — never the raw token or the hash
        let list = db.list_mcp_tokens(7);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, id);
        assert_eq!(list[0].1.as_deref(), Some("ci"));
        let prefix = list[0].2.as_deref().expect("prefix stored");
        assert_eq!(prefix.len(), 8);
        assert!(raw.starts_with(prefix), "prefix is a prefix, not the token");
        assert_eq!(list[0].4, 0);
        assert!(
            db.list_mcp_tokens(8).is_empty(),
            "other sellers see nothing"
        );

        // scoped revoke: wrong seller loses, owner and admin win, idempotent
        assert!(!db.revoke_mcp_token(id, Some(8)));
        assert!(db.revoke_mcp_token(id, Some(7)));
        assert_eq!(db.get_mcp_token_by_hash(&sha256_hex(&raw)).unwrap().3, 1);
        assert!(db.revoke_mcp_token(id, Some(7)), "idempotent");
        assert!(!db.revoke_mcp_token(999_999, None), "unknown id");
        let (id2, raw2) = db.create_mcp_token(7, None);
        assert!(
            db.revoke_mcp_token(id2, None),
            "admin scope acts on any row"
        );
        assert_eq!(db.get_mcp_token_by_hash(&sha256_hex(&raw2)).unwrap().3, 1);

        // touch stamps last_used_at (best-effort bookkeeping)
        db.touch_mcp_token(id);
        {
            let conn = Connection::open(&path).unwrap();
            let used: Option<String> = conn
                .query_row(
                    "SELECT last_used_at FROM mcp_tokens WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(used.is_some());
        }
    }

    #[test]
    fn test_mcp_audit_carries_seller_id() {
        let (db, path) = booking_db("audit");
        db.mcp_audit("list_plans", "seller", Some(9), true, None);
        db.mcp_audit("owner_tool", "owner", None, false, Some("boom"));
        let conn = Connection::open(&path).unwrap();
        let mut stmt = conn
            .prepare("SELECT role, seller_id, ok FROM mcp_audit ORDER BY id")
            .unwrap();
        let rows: Vec<(String, Option<i64>, i64)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("seller".to_string(), Some(9), 1));
        assert_eq!(rows[1], ("owner".to_string(), None, 0));
    }

    #[test]
    fn test_revenue_by_asset_per_asset_lines() {
        let (db, _path) = booking_db("revasset");
        db.save_invoice("i1", 1, "50", "USDT", None, "paid", None, None, None, None);
        db.save_invoice("i2", 1, "20", "USDT", None, "paid", None, None, None, None);
        db.save_invoice("i3", 1, "0.5", "BTC", None, "paid", None, None, None, None);
        // unpaid invoices stay out of the per-asset lines
        db.save_invoice(
            "i4", 1, "100", "USDT", None, "active", None, None, None, None,
        );
        let rows = db.revenue_by_asset();
        assert_eq!(
            rows,
            vec![("BTC".to_string(), 0.5), ("USDT".to_string(), 70.0)]
        );
        // revenue_total's cross-asset behavior is deliberately unchanged
        // (L5 ruling 5: the sum is a display defect handled at the callers)
        assert!((db.revenue_total(None) - 70.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn test_web_account_create_verify() {
        let path = "/tmp/opencode/test_web_account_create_verify.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_web_account("alice", "secret", "admin", None)
            .await
            .unwrap();
        // correct password
        let r = db.verify_web_password("alice", "secret").await;
        assert!(r.is_some());
        let (id, role, seller) = r.unwrap();
        assert!(id > 0);
        assert_eq!(role, "admin");
        assert!(seller.is_none());
        // wrong password
        assert!(db.verify_web_password("alice", "wrong").await.is_none());
        // duplicate username errors
        let dup = db.create_web_account("alice", "x", "user", None).await;
        assert!(dup.is_err());
    }

    #[tokio::test]
    async fn test_web_account_update_password() {
        let path = "/tmp/opencode/test_web_account_update_password.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_web_account("bob", "oldpw", "user", None)
            .await
            .unwrap();
        assert!(db.verify_web_password("bob", "oldpw").await.is_some());
        db.update_web_password("bob", "newpw").await.unwrap();
        // old fails
        assert!(db.verify_web_password("bob", "oldpw").await.is_none());
        // new succeeds
        assert!(db.verify_web_password("bob", "newpw").await.is_some());
    }

    #[tokio::test]
    async fn test_web_session_lifecycle() {
        let path = "/tmp/opencode/test_web_session_lifecycle.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_web_account("carol", "pw", "seller", Some(42))
            .await
            .unwrap();
        let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("carol").unwrap();
        // create session (long-lived)
        let token = db.create_web_session(acc_id, "seller", Some(42), 7);
        assert!(!token.is_empty());
        // get returns account/role/seller
        let got = db.get_web_session(&token);
        assert!(got.is_some());
        let (a, role, seller) = got.unwrap();
        assert_eq!(a, acc_id);
        assert_eq!(role, "seller");
        assert_eq!(seller, Some(42));
        // delete removes
        db.delete_web_session(&token);
        assert!(db.get_web_session(&token).is_none());
        // expired: days=0
        let token2 = db.create_web_session(acc_id, "seller", Some(42), 0);
        assert!(db.get_web_session(&token2).is_none());
    }

    #[tokio::test]
    async fn test_list_web_accounts() {
        let path = "/tmp/opencode/test_list_web_accounts.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_web_account("u1", "p1", "owner", None)
            .await
            .unwrap();
        db.create_web_account("u2", "p2", "admin", None)
            .await
            .unwrap();
        let list = db.list_web_accounts();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], ("u1".to_string(), "owner".to_string()));
        assert_eq!(list[1], ("u2".to_string(), "admin".to_string()));
    }

    #[test]
    fn test_seller_upsert_and_channel() {
        let path = "/tmp/opencode/test_seller_upsert_and_channel.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let s = db.get_seller(1).unwrap();
        assert_eq!(s.0, 1);
        assert_eq!(s.1, Some("alice".to_string()));
        assert_eq!(s.2, Some("@chan".to_string()));
        assert_eq!(db.seller_channel(1).as_deref(), Some("@chan"));
        db.set_seller_channel(1, "@chan2");
        assert_eq!(db.seller_channel(1).as_deref(), Some("@chan2"));
        assert!(db.list_sellers().contains(&1));
    }

    #[test]
    fn test_plan_crud_and_scoping() {
        let path = "/tmp/opencode/test_plan_crud_and_scoping.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.add_plan(1, "P", "1", "USDT", 7, Some("@chan"));
        let scoped = db.list_plans(Some(1));
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].0, "P");
        let global = db.list_plans(None);
        assert_eq!(global.len(), 1);
        let got = db.get_plan(1, "P").unwrap();
        assert_eq!(got.3, "P");
        db.remove_plan(1, "P");
        assert_eq!(db.list_plans(Some(1)).len(), 0);
    }

    #[test]
    fn test_subscription_create_and_user_has_active() {
        let path = "/tmp/opencode/test_subscription_create_and_user_has_active.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let id = db.create_subscription(1, 42, None, "@chan", 7, None, None);
        assert!(id > 0);
        assert!(db.user_has_active_sub(1, 42, "@chan"));
        assert!(!db.user_has_active_sub(1, 999, "@chan"));
        assert!(!db.user_has_active_sub(2, 42, "@chan"));
    }

    #[test]
    fn test_activation_code_create_redeem() {
        let path = "/tmp/opencode/test_activation_code_create_redeem.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let code = db.create_code(1, None, Some(7), 1, None, 12);
        assert_eq!(code.len(), 12);
        let got = db.get_code(&code).unwrap();
        assert_eq!(got.1, 1);
        assert_eq!(got.5, 0);
        let res = db.redeem_code(&code, 42);
        assert!(res.is_ok());
        let got2 = db.get_code(&code).unwrap();
        assert_eq!(got2.5, 1);
        // second redeem fails
        let res2 = db.redeem_code(&code, 42);
        assert!(res2.is_err());
    }

    #[test]
    fn test_invoice_save_and_pending() {
        let path = "/tmp/opencode/test_invoice_save_and_pending.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.save_invoice(
            "inv1",
            1,
            "10",
            "USDT",
            None,
            "active",
            None,
            Some("P"),
            Some(42),
            None,
        );
        let inv = db.get_invoice("inv1").unwrap();
        assert_eq!(inv.0, "10");
        assert_eq!(inv.1, "USDT");
        assert_eq!(inv.2, "active");
        let pending = db.pending_invoices();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].0, "inv1");
    }

    #[test]
    fn test_invoice_claim_finalize_and_exists() {
        let path = "/tmp/opencode/test_invoice_claim_finalize_and_exists.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.save_invoice(
            "inv_claim",
            1,
            "10",
            "USDT",
            None,
            "active",
            None,
            Some("P"),
            Some(42),
            None,
        );
        // claim succeeds exactly once (atomic handoff between granters)
        assert!(db.claim_invoice_for_grant("inv_claim"));
        assert!(!db.claim_invoice_for_grant("inv_claim"));
        // a FRESH claim is never re-queued: the live claimer owns the row
        assert!(!db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
        // ... but a claim wedged past WEDGED_GRANT_STALE_MINUTES (claimer
        // crashed between claim and finalize) re-queues for re-verification,
        // which is safe because both grant branches are existence-guarded.
        {
            let conn = db.conn.lock().unwrap_or_else(|p| p.into_inner());
            let stale = (Utc::now()
                - chrono::Duration::try_minutes(WEDGED_GRANT_STALE_MINUTES + 1)
                    .unwrap_or_default())
            .to_rfc3339();
            let _ = conn.execute(
                "UPDATE invoices SET updated_at=?1 WHERE invoice_id='inv_claim'",
                params![stale],
            );
        }
        assert!(db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
        // release_invoice_claim unwinds ONLY a 'granting' row, exactly once
        assert!(db.release_invoice_claim("inv_claim"));
        assert!(!db.release_invoice_claim("inv_claim"));
        assert_eq!(db.get_invoice("inv_claim").unwrap().2, "active");
        assert!(db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
        // re-claim, then the normal grant + finalize flow drops it for good
        assert!(db.claim_invoice_for_grant("inv_claim"));
        // no subscription yet
        assert!(!db.subscription_exists_for_invoice("inv_claim"));
        db.create_subscription(1, 42, None, "@chan", 7, Some("inv_claim"), None);
        assert!(db.subscription_exists_for_invoice("inv_claim"));
        // finalize flips to paid and drops it out of the pending queue
        db.finalize_invoice_paid("inv_claim");
        assert_eq!(db.get_invoice("inv_claim").unwrap().2, "paid");
        assert!(!db.pending_invoices().iter().any(|r| r.0 == "inv_claim"));
        // a paid invoice can no longer be claimed, nor released
        assert!(!db.claim_invoice_for_grant("inv_claim"));
        assert!(!db.release_invoice_claim("inv_claim"));
    }

    #[test]
    fn test_seller_has_channel() {
        let path = "/tmp/opencode/test_seller_has_channel.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        // unknown seller: no channel
        assert!(!db.seller_has_channel(1));
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        assert!(db.seller_has_channel(1));
        // an empty-string channel reads as NO channel (matches the
        // seller_channel(..).unwrap_or_default().is_empty() callers do)
        db.upsert_seller(2, Some("bob"), Some(""));
        assert!(!db.seller_has_channel(2));
        assert!(db.seller_channel(2).is_some());
    }

    #[test]
    fn test_pending_invoices_includes_products_only() {
        let path = "/tmp/opencode/test_pending_invoices_includes_products_only.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        // Product invoice (plan_id NULL, product_id set): must be queued.
        db.save_invoice(
            "inv_prod",
            1,
            "5",
            "USDT",
            None,
            "active",
            None,
            None,
            Some(9),
            Some(3),
        );
        // Plan invoice: queued as before.
        db.save_invoice(
            "inv_plan",
            1,
            "5",
            "USDT",
            None,
            "active",
            None,
            Some("gold"),
            Some(9),
            None,
        );
        // Neither plan nor product: nothing grantable, never queued.
        db.save_invoice(
            "inv_bare",
            1,
            "5",
            "USDT",
            None,
            "active",
            None,
            None,
            Some(9),
            None,
        );
        let pending = db.pending_invoices();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().any(|r| r.0 == "inv_prod"));
        assert!(pending.iter().any(|r| r.0 == "inv_plan"));
        assert!(!pending.iter().any(|r| r.0 == "inv_bare"));
    }

    #[test]
    fn test_product_add_get_list_and_collisions() {
        let path = "/tmp/opencode/test_product_add_get_list_and_collisions.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.upsert_seller(2, Some("bob"), Some("@chan"));
        // invalid kind rejected
        assert!(
            db.add_product(1, "bad", "5", "USDT", "service", None, None)
                .is_err()
        );
        let id = db
            .add_product(
                1,
                "ebook",
                "5",
                "USDT",
                "digital_link",
                Some("https://example.com/dl"),
                Some(10),
            )
            .unwrap();
        assert!(id > 0);
        let p = db.get_product(1, "ebook").unwrap();
        assert_eq!(p.0, id);
        assert_eq!(p.1, 1);
        assert_eq!(p.2, "ebook");
        assert_eq!(p.3, "5");
        assert_eq!(p.4, "USDT");
        assert_eq!(p.5, "digital_link");
        assert_eq!(p.6.as_deref(), Some("https://example.com/dl"));
        assert_eq!(p.7, Some(10));
        assert_eq!(p.8, 1);
        // same name, same seller: product-vs-product collision
        assert!(
            db.add_product(1, "ebook", "9", "TON", "digital_file", None, None)
                .is_err()
        );
        // same name, other seller: fine
        assert!(
            db.add_product(2, "ebook", "5", "USDT", "digital_link", None, None)
                .is_ok()
        );
        // product-vs-plan collision, scoped to the same seller only
        db.add_plan(1, "gold", "10", "USDT", 30, Some("@chan"));
        assert!(
            db.add_product(1, "gold", "5", "USDT", "digital_link", None, None)
                .is_err()
        );
        assert!(
            db.add_product(2, "gold", "5", "USDT", "digital_link", None, None)
                .is_ok()
        );
        // unscoped listing is the active-only shop view
        assert_eq!(db.list_products(None).len(), 3);
        db.set_product_active(1, "ebook", false);
        assert_eq!(db.list_products(None).len(), 2);
        assert!(db.list_products(None).iter().all(|r| r.8 == 1));
        // scoped listing is the seller's management view: retired rows stay
        assert_eq!(db.list_products(Some(1)).len(), 1);
        assert_eq!(db.list_products(Some(1))[0].8, 0);
        // scoped /buy name resolution (active only): the live 'ebook' belongs
        // to seller 2 (seller 1's copy is retired), the plan 'gold' to
        // seller 1 — each namespace resolves only its own rows.
        assert_eq!(db.find_products_by_name(Some(2), "ebook").len(), 1);
        assert_eq!(db.find_products_by_name(Some(2), "ebook")[0].1, 2);
        assert_eq!(db.find_products_by_name(Some(1), "ebook").len(), 0);
        assert_eq!(db.find_plans_by_name(Some(1), "gold").len(), 1);
        assert_eq!(db.find_plans_by_name(Some(1), "gold")[0].6, 1);
        // unscoped is the cross-seller shop view (the caller filters
        // namespaces: seller_id 0 is admin/owner-only, see commands.rs)
        assert_eq!(db.find_products_by_name(None, "ebook").len(), 1);
        // a retired product's name slot can be reused (upsert revives it)
        let id2 = db
            .add_product(1, "ebook", "7", "TON", "physical", None, Some(3))
            .unwrap();
        let p2 = db.get_product(1, "ebook").unwrap();
        assert_eq!(p2.0, id2);
        assert_eq!(p2.3, "7");
        assert_eq!(p2.8, 1);
        assert_eq!(db.list_products(None).len(), 3);
    }

    #[test]
    fn test_product_grant_once_and_stock_exhaustion() {
        let path = "/tmp/opencode/test_product_grant_once_and_stock.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let pid = db
            .add_product(
                1,
                "course",
                "5",
                "USDT",
                "digital_link",
                Some("link"),
                Some(2),
            )
            .unwrap();
        // first grant succeeds and decrements stock
        assert!(db.grant_product(1, 42, pid, "inv_a").unwrap());
        assert_eq!(db.get_product(1, "course").unwrap().7, Some(1));
        assert!(db.product_grant_exists("inv_a"));
        // same invoice again: idempotent no-op, stock untouched
        assert!(!db.grant_product(1, 42, pid, "inv_a").unwrap());
        assert_eq!(db.get_product(1, "course").unwrap().7, Some(1));
        // second unit sells, stock hits zero
        assert!(db.grant_product(1, 43, pid, "inv_b").unwrap());
        assert_eq!(db.get_product(1, "course").unwrap().7, Some(0));
        // exhausted: no grant row, no further decrement
        assert_eq!(
            db.grant_product(1, 44, pid, "inv_c").unwrap_err(),
            "out of stock"
        );
        assert!(!db.product_grant_exists("inv_c"));
        assert_eq!(db.get_product(1, "course").unwrap().7, Some(0));
        // NULL stock = unlimited, never exhausts
        let uid = db
            .add_product(
                1,
                "bonus",
                "1",
                "TON",
                "digital_file",
                Some("file_id"),
                None,
            )
            .unwrap();
        for i in 0..5 {
            assert!(
                db.grant_product(1, 50 + i, uid, &format!("inv_u{}", i))
                    .unwrap()
            );
        }
        assert_eq!(db.get_product(1, "bonus").unwrap().7, None);
    }

    #[test]
    fn test_order_create_list_status_and_scoping() {
        let path = "/tmp/opencode/test_order_create_list_status.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let mug = db
            .add_product(1, "mug", "9", "USDT", "physical", None, None)
            .unwrap();
        let tee = db
            .add_product(2, "tee", "7", "USDT", "physical", None, None)
            .unwrap();
        // create -> 8-char order id, pending, no tracking
        let oid = db.create_order(1, mug, 42, "inv_o1").unwrap();
        assert_eq!(oid.len(), 8);
        db.create_order(2, tee, 43, "inv_o2").unwrap();
        // one order per purchase (UNIQUE invoice_id)
        assert!(db.create_order(1, mug, 42, "inv_o1").is_err());
        // list: all vs seller-scoped
        assert_eq!(db.list_orders(None).len(), 2);
        let own = db.list_orders(Some(1));
        assert_eq!(own.len(), 1);
        assert_eq!(own[0].0, oid);
        assert_eq!(own[0].1, 1);
        assert_eq!(own[0].2, 42);
        assert_eq!(own[0].3, "pending");
        assert_eq!(own[0].4, None);
        assert!(!own[0].5.is_empty());
        // status transitions
        assert!(db.set_order_status(1, &oid, "shipped"));
        assert_eq!(db.list_orders(Some(1))[0].3, "shipped");
        assert!(db.set_order_status(1, &oid, "delivered"));
        assert_eq!(db.list_orders(Some(1))[0].3, "delivered");
        assert!(db.set_order_status(1, &oid, "refunded"));
        assert!(db.set_order_status(1, &oid, "cancelled"));
        assert!(db.set_order_status(1, &oid, "pending"));
        // invalid status rejected, state untouched
        assert!(!db.set_order_status(1, &oid, "destroyed"));
        assert!(!db.set_order_status(1, &oid, ""));
        assert_eq!(db.list_orders(Some(1))[0].3, "pending");
        // seller scoping: seller 2 cannot flip seller 1's order
        assert!(!db.set_order_status(2, &oid, "shipped"));
        assert_eq!(db.list_orders(Some(1))[0].3, "pending");
        // unknown order id
        assert!(!db.set_order_status(1, "nope1234", "shipped"));
        // tracking, same seller scoping
        db.set_order_tracking(1, &oid, "TRACK123");
        assert_eq!(db.list_orders(Some(1))[0].4.as_deref(), Some("TRACK123"));
        assert_eq!(db.list_orders(Some(2))[0].4, None);
    }

    #[test]
    fn test_invoice_product_roundtrip() {
        let path = "/tmp/opencode/test_invoice_product_roundtrip.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let pid = db
            .add_product(
                1,
                "guide",
                "2",
                "TON",
                "digital_file",
                Some("FILEID123"),
                None,
            )
            .unwrap();
        db.save_invoice(
            "inv_p",
            1,
            "2",
            "TON",
            None,
            "active",
            None,
            None,
            Some(42),
            Some(pid),
        );
        assert_eq!(db.invoice_product("inv_p"), Some(pid));
        let (dp, kind, name, delivery) = db.invoice_product_delivery("inv_p").unwrap();
        assert_eq!(dp, pid);
        assert_eq!(kind, "digital_file");
        assert_eq!(name, "guide");
        assert_eq!(delivery.as_deref(), Some("FILEID123"));
        // plan invoices (product_id NULL) take the existing plan path
        db.save_invoice(
            "inv_plan",
            1,
            "10",
            "USDT",
            None,
            "active",
            None,
            Some("gold"),
            Some(42),
            None,
        );
        assert_eq!(db.invoice_product("inv_plan"), None);
        assert!(db.invoice_product_delivery("inv_plan").is_none());
        assert!(db.invoice_product("missing").is_none());
        assert!(db.invoice_product_delivery("missing").is_none());
    }

    #[test]
    fn test_subscription_status_and_by_invoice() {
        let path = "/tmp/opencode/test_subscription_status_and_by_invoice.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let id = db.create_subscription(1, 42, None, "@chan", 7, Some("inv_s"), None);
        assert_eq!(
            db.subscription_by_invoice("inv_s"),
            Some((id, "@chan".to_string()))
        );
        assert!(db.subscription_by_invoice("nope").is_none());
        db.set_subscription_status(id, "refunded");
        assert_eq!(db.list_subscriptions(Some(1), 10)[0].4, "refunded");
    }

    #[test]
    fn test_list_subscriptions_filtered() {
        let path = "/tmp/opencode/test_list_subscriptions_filtered.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let id1 = db.create_subscription(1, 42, None, "@chan", 7, Some("inv_a"), None);
        let id2 = db.create_subscription(1, 43, None, "@chan", 7, Some("inv_b"), None);
        db.set_subscription_status(id2, "expired");
        // status filter
        let active = db.list_subscriptions_filtered(Some(1), Some("active"), 10);
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0, id1);
        let expired = db.list_subscriptions_filtered(Some(1), Some("expired"), 10);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, id2);
        // no status filter returns all
        let all = db.list_subscriptions_filtered(Some(1), None, 10);
        assert_eq!(all.len(), 2);
        // global scope
        let global = db.list_subscriptions_filtered(None, None, 10);
        assert_eq!(global.len(), 2);
        // order by id desc
        assert!(global[0].0 > global[1].0);
        // limit
        let limited = db.list_subscriptions_filtered(Some(1), None, 1);
        assert_eq!(limited.len(), 1);
        let _ = id1;
    }

    #[test]
    fn test_refund_and_payout_state_transitions() {
        let path = "/tmp/opencode/test_refund_and_payout_state_transitions.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.upsert_seller(2, Some("bob"), Some("@chan"));
        db.save_invoice(
            "inv_r1",
            1,
            "10",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(42),
            None,
        );
        // refund starts pending; unknown user falls back to the invoice buyer
        let rid = db.create_refund(1, Some("inv_r1"), None, "10", "USDT", Some("damaged"));
        assert!(rid > 0);
        let refunds = db.list_refunds(Some(1));
        assert_eq!(refunds.len(), 1);
        assert_eq!(refunds[0].0, rid);
        assert_eq!(refunds[0].1, 1);
        assert_eq!(refunds[0].2.as_deref(), Some("inv_r1"));
        assert_eq!(refunds[0].3, Some(42));
        assert_eq!(refunds[0].4, "10");
        assert_eq!(refunds[0].5, "USDT");
        assert_eq!(refunds[0].6.as_deref(), Some("damaged"));
        assert_eq!(refunds[0].7, "pending");
        // pending -> approved -> executed
        db.set_refund_status(rid, "approved");
        assert_eq!(db.list_refunds(Some(1))[0].7, "approved");
        db.set_refund_status(rid, "executed");
        assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executed");
        // explicit user + scoped listing
        let rid2 = db.create_refund(2, None, Some(7), "3", "TON", None);
        assert!(rid2 > 0);
        assert_eq!(db.list_refunds(Some(2))[0].3, Some(7));
        assert_eq!(db.list_refunds(None).len(), 2);
        // payout pending -> approved -> paid
        let pid = db.create_payout(1, "5", "USDT", Some("@alice"));
        assert!(pid > 0);
        assert_eq!(db.list_payouts(Some(1))[0].5, "pending");
        db.set_payout_status(pid, "approved");
        assert_eq!(db.list_payouts(Some(1))[0].5, "approved");
        db.set_payout_status(pid, "paid");
        assert_eq!(db.get_payout(pid.to_string()).unwrap().5, "paid");
        assert_eq!(db.list_payouts(Some(2)).len(), 0);
    }

    /// CAS execution claims on payouts: begin only wins from 'approved',
    /// exactly one concurrent claimer wins, finish only applies from
    /// 'executing', and invalid finish statuses are rejected with no write.
    #[test]
    fn test_payout_execution_cas_transitions() {
        let path = "/tmp/opencode/test_payout_execution_cas_transitions.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));

        // begin-from-pending is refused; begin-from-approved wins
        let pid = db.create_payout(1, "5", "USDT", Some("@alice"));
        assert_eq!(
            db.get_payout_status(pid.to_string()).as_deref(),
            Some("pending")
        );
        assert!(!db.begin_payout_execution(pid.to_string()));
        db.set_payout_status(pid, "approved");
        assert!(db.begin_payout_execution(pid.to_string()));
        assert_eq!(
            db.get_payout_status(pid.to_string()).as_deref(),
            Some("executing")
        );

        // double-begin loses: the row is no longer 'approved'
        let pid2 = db.create_payout(1, "7", "USDT", Some("@alice"));
        db.set_payout_status(pid2, "approved");
        assert!(db.begin_payout_execution(pid2.to_string()));
        assert!(!db.begin_payout_execution(pid2.to_string()));

        // finish applies only from 'executing'
        assert_eq!(db.finish_payout_execution(pid.to_string(), "paid"), 1);
        assert_eq!(
            db.get_payout_status(pid.to_string()).as_deref(),
            Some("paid")
        );
        // finish-from-wrong-status (already 'paid') changes nothing
        assert_eq!(db.finish_payout_execution(pid.to_string(), "approved"), 0);
        assert_eq!(db.finish_payout_execution(pid.to_string(), "paid"), 0);

        // revert path: 'executing' -> 'approved' is legal, then re-executable
        assert_eq!(db.finish_payout_execution(pid2.to_string(), "approved"), 1);
        assert_eq!(
            db.get_payout_status(pid2.to_string()).as_deref(),
            Some("approved")
        );
        assert!(db.begin_payout_execution(pid2.to_string()));
        assert_eq!(db.finish_payout_execution(pid2.to_string(), "paid"), 1);

        // invalid status args are rejected with no DB write (row stays
        // 'executing'); matching is case-sensitive by design
        let pid3 = db.create_payout(1, "9", "USDT", Some("@alice"));
        db.set_payout_status(pid3, "approved");
        assert!(db.begin_payout_execution(pid3.to_string()));
        for bad in ["failed", "processed", "pending", "executing", "PAID", ""] {
            assert_eq!(db.finish_payout_execution(pid3.to_string(), bad), 0);
        }
        assert_eq!(
            db.get_payout_status(pid3.to_string()).as_deref(),
            Some("executing")
        );
    }

    /// CAS execution claims on refunds: same contract as payouts, with
    /// "executed" as the terminal outcome.
    #[test]
    fn test_refund_execution_cas_transitions() {
        let path = "/tmp/opencode/test_refund_execution_cas_transitions.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));

        // begin-from-pending is refused; begin-from-approved wins
        let rid = db.create_refund(1, None, Some(42), "10", "USDT", Some("damaged"));
        assert!(!db.begin_refund_execution(rid.to_string()));
        db.set_refund_status(rid, "approved");
        assert!(db.begin_refund_execution(rid.to_string()));
        assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executing");

        // double-begin loses: the row is no longer 'approved'
        let rid2 = db.create_refund(1, None, Some(43), "3", "TON", None);
        db.set_refund_status(rid2, "approved");
        assert!(db.begin_refund_execution(rid2.to_string()));
        assert!(!db.begin_refund_execution(rid2.to_string()));

        // finish applies only from 'executing'
        assert_eq!(db.finish_refund_execution(rid.to_string(), "executed"), 1);
        assert_eq!(db.get_refund(rid.to_string()).unwrap().7, "executed");
        // finish-from-wrong-status (already 'executed') changes nothing
        assert_eq!(db.finish_refund_execution(rid.to_string(), "approved"), 0);
        assert_eq!(db.finish_refund_execution(rid.to_string(), "executed"), 0);

        // revert path: 'executing' -> 'approved' is legal, then re-executable
        assert_eq!(db.finish_refund_execution(rid2.to_string(), "approved"), 1);
        assert_eq!(db.get_refund(rid2.to_string()).unwrap().7, "approved");
        assert!(db.begin_refund_execution(rid2.to_string()));
        assert_eq!(db.finish_refund_execution(rid2.to_string(), "executed"), 1);

        // invalid status args are rejected with no DB write (row stays
        // 'executing'); matching is case-sensitive by design
        let rid3 = db.create_refund(1, None, Some(44), "1", "USDT", None);
        db.set_refund_status(rid3, "approved");
        assert!(db.begin_refund_execution(rid3.to_string()));
        for bad in ["paid", "failed", "processed", "pending", "EXECUTED", ""] {
            assert_eq!(db.finish_refund_execution(rid3.to_string(), bad), 0);
        }
        assert_eq!(db.get_refund(rid3.to_string()).unwrap().7, "executing");
    }

    proptest! {
        /// Model check of the invoice claim state machine: for any sequence of
        /// claim / finalize / reset operations, the database's observable state
        /// always matches the reference model (active → granting → paid, reset
        /// re-activates) and a claim succeeds only when the model is 'active'.
        #[test]
        fn prop_claim_state_machine(ops in proptest::collection::vec(0u8..3, 1..40)) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("claim.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            db.upsert_seller(1, Some("s"), Some("@chan"));
            db.save_invoice(
                "inv",
                1,
                "10",
                "USDT",
                None,
                "active",
                None,
                Some("P"),
                Some(42),
                None,
            );
            let mut model = 0u8; // 0=active, 1=granting, 2=paid
            for op in ops {
                match op {
                    0 => {
                        let won = db.claim_invoice_for_grant("inv");
                        prop_assert_eq!(won, model == 0);
                        if model == 0 {
                            model = 1;
                        }
                    }
                    1 => {
                        db.finalize_invoice_paid("inv");
                        model = 2;
                    }
                    _ => {
                        db.update_invoice_status("inv", "active");
                        model = 0;
                    }
                }
                let status = db.get_invoice("inv").unwrap().2.clone();
                let want = match model {
                    0 => "active",
                    1 => "granting",
                    _ => "paid",
                };
                prop_assert_eq!(status, want);
            }
        }

        /// Any i64 plan duration survives create_subscription without panicking.
        #[test]
        fn prop_subscription_arbitrary_days(
            seller in 1i64..1000,
            user in 1i64..1000,
            days in any::<i64>(),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sub.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            db.upsert_seller(seller, Some("s"), Some("@chan"));
            let id = db.create_subscription(seller, user, None, "@chan", days, None, None);
            prop_assert!(id > 0);
        }
    }

    proptest! {
        /// Product grant lifecycle: no matter how many grant attempts race in,
        /// an invoice's product is granted at most once (double-grant guard),
        /// and exhaustion is reported rather than overselling.
        #[test]
        fn prop_product_grant_exactly_once(
            attempts in 1u8..8,
            stock in proptest::option::of(0i64..3),
        ) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("pg.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            db.upsert_seller(7, Some("s"), Some("@chan"));
            let pid = db
                .add_product(7, "Ebook", "5", "USDT", "digital_link", Some("https://x/dl"), stock)
                .unwrap();
            db.save_invoice(
                "invP",
                7,
                "5",
                "USDT",
                None,
                "active",
                None,
                None,
                Some(9),
                None,
            );
            let mut granted = 0;
            for _ in 0..attempts {
                match db.grant_product(7, 9, pid, "invP") {
                    Ok(true) => granted += 1,
                    Ok(false) => {}
                    Err(_) => break, // out of stock
                }
            }
            prop_assert!(granted <= 1);
            prop_assert_eq!(db.product_grant_exists("invP"), granted == 1);
        }
    }

    #[test]
    fn test_subscription_does_not_panic_on_huge_days() {
        let path = "/tmp/opencode/test_subscription_does_not_panic_on_huge_days.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let id = db.create_subscription(1, 42, None, "@chan", i64::MAX, None, None);
        assert!(id > 0);
        // extreme negative duration must not panic either
        let id2 = db.create_subscription(1, 43, None, "@chan", -i64::MAX, None, None);
        assert!(id2 > 0);
        let subs = db.list_subscriptions(Some(1), 10);
        assert_eq!(subs.len(), 2);
    }

    #[tokio::test]
    async fn test_one_time_credential_rotation() {
        let path = "/tmp/opencode/test_one_time_credential_rotation.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_web_account("rot", "pw", "admin", None)
            .await
            .unwrap();
        let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("rot").unwrap();
        let token = db.create_web_session(acc_id, "admin", None, 7);
        assert!(db.get_web_session(&token).is_some());
        // Simulate a pre-CSPRNG database: clear the rotation marker.
        {
            let conn = db
                .conn
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            conn.execute(
                "DELETE FROM config WHERE key='migration_rotate_tokens_v1'",
                [],
            )
            .unwrap();
        }
        // Re-opening rotates legacy credentials exactly once.
        let db2 = Db::new(path).unwrap();
        assert!(db2.get_web_session(&token).is_none());
        // Sessions created after the rotation survive later restarts.
        let token2 = db2.create_web_session(acc_id, "admin", None, 7);
        assert!(db2.get_web_session(&token2).is_some());
        let db3 = Db::new(path).unwrap();
        assert!(db3.get_web_session(&token2).is_some());
    }

    #[test]
    fn test_duplicate_invoice_grants_do_not_break_startup() {
        let path = "/tmp/opencode/test_duplicate_invoice_grants.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.create_subscription(1, 42, None, "@c", 7, Some("dup"), None);
        db.create_subscription(1, 43, None, "@c", 7, Some("dup"), None);
        // Re-open: the unique index cannot be built over duplicates, but
        // startup must continue.
        let db2 = Db::new(path).unwrap();
        assert!(db2.subscription_exists_for_invoice("dup"));
    }

    #[test]
    fn test_admin_add_remove() {
        let path = "/tmp/opencode/test_admin_add_remove.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        let owner_id = 999i64;
        assert!(!db.is_admin(5, owner_id));
        db.add_admin(5, Some("admin5"));
        assert!(db.is_admin(5, owner_id));
        assert!(db.list_admins().contains(&5));
        db.remove_admin(5);
        assert!(!db.is_admin(5, owner_id));
        assert!(!db.list_admins().contains(&5));
    }

    #[test]
    fn test_expired_subscriptions() {
        let path = "/tmp/opencode/test_expired_subscriptions.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        let id = db.create_subscription(1, 42, None, "@chan", -1, None, None);
        let expired = db.expired_subscriptions();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0, id);
        db.mark_subscription_expired(id);
        let subs = db.list_subscriptions(Some(1), 10);
        assert_eq!(subs[0].4, "expired");
    }

    #[test]
    fn test_ai_template_upsert_get_list_delete() {
        let path = "/tmp/opencode/test_ai_template_upsert_get_list_delete.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.upsert_ai_template(
            1,
            "t1",
            Some("desc"),
            Some("sys"),
            "user prompt",
            Some("vars"),
        );
        let got = db.get_ai_template(1, "t1").unwrap();
        assert_eq!(got.0, "t1");
        assert_eq!(got.1, Some("desc".to_string()));
        assert_eq!(got.3, "user prompt");
        let list = db.list_ai_templates(1);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "t1");
        // upsert update
        db.upsert_ai_template(1, "t1", Some("desc2"), Some("sys2"), "new prompt", None);
        let got2 = db.get_ai_template(1, "t1").unwrap();
        assert_eq!(got2.1, Some("desc2".to_string()));
        assert_eq!(got2.3, "new prompt");
        db.delete_ai_template(1, "t1");
        assert!(db.get_ai_template(1, "t1").is_none());
        assert!(db.list_ai_templates(1).is_empty());
    }

    #[test]
    fn test_ai_usage_record_and_summary() {
        let path = "/tmp/opencode/test_ai_usage_record_and_summary.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.record_ai_usage(
            1,
            Some(42),
            "openai",
            "gpt",
            Some("t1"),
            10,
            5,
            15,
            0.1,
            "ok",
            None,
        );
        assert_eq!(db.ai_usage_today_tokens(1), 15);
        let summary = db.ai_usage_summary(1, 7);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].1, 15);
        assert_eq!(summary[0].2, 1);
    }

    #[test]
    fn test_ai_usage_global_and_recent_errors() {
        let path = "/tmp/opencode/test_ai_usage_global_and_recent_errors.db";
        let _ = fs::remove_file(path);
        let db = Db::new(path).unwrap();
        db.upsert_seller(1, Some("alice"), Some("@chan"));
        db.record_ai_usage(
            1,
            Some(42),
            "openai",
            "gpt",
            Some("t1"),
            10,
            5,
            15,
            0.1,
            "ok",
            None,
        );
        db.record_ai_usage(
            1,
            Some(42),
            "openai",
            "gpt",
            Some("t1"),
            1,
            1,
            2,
            0.01,
            "error",
            Some("boom"),
        );
        assert_eq!(db.ai_usage_today_tokens_global(), 17);
        let gsummary = db.ai_usage_summary_global(7);
        assert_eq!(gsummary.len(), 1);
        assert_eq!(gsummary[0].1, 17);
        assert_eq!(gsummary[0].2, 2);
        let cost = db.ai_usage_cost_total_global(7);
        assert!((cost - 0.11).abs() < 1e-9);
        // global recent errors
        let errs = db.ai_usage_recent_errors_global(10);
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].4, "error");
        assert_eq!(errs[0].3, Some("boom".to_string()));
        // seller scoped
        let serrs = db.ai_usage_recent_errors(1, 10);
        assert_eq!(serrs.len(), 1);
        assert_eq!(serrs[0].1, "openai");
        // no errors for other seller
        assert!(db.ai_usage_recent_errors(2, 10).is_empty());
    }

    #[test]
    fn test_scheduled_post_create_and_list_due() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sched.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let id = db.create_scheduled_post(1, "chat", "content", "text", &past);
        assert!(id > 0);
        let due = db.list_due_scheduled();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, id);
        db.mark_scheduled_posted(id, 999).unwrap();
        let due2 = db.list_due_scheduled();
        assert!(due2.is_empty());
    }

    #[test]
    fn test_scheduled_post_attempts_cap_terminal() {
        // Bounded retries (2026-10-25 review): every send failure bumps the
        // counter; at the cap the post leaves the due queue with status
        // 'failed' instead of retrying forever. A concurrent 'posted' win is
        // never overwritten by the terminal mark.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sched_cap.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let id = db.create_scheduled_post(1, "chat", "content", "text", &past);
        for expected in 1..=10 {
            assert_eq!(db.bump_scheduled_attempts(id), expected);
        }
        // Not terminal yet: still due.
        assert_eq!(db.list_due_scheduled().len(), 1);
        db.mark_scheduled_failed(id);
        assert!(db.list_due_scheduled().is_empty());
        let all = db.list_scheduled_posts(Some(1));
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].6, "failed");
        // The terminal mark is scoped to 'pending': a posted row stays.
        let id2 = db.create_scheduled_post(1, "chat", "content", "text", &past);
        db.mark_scheduled_posted(id2, 42).unwrap();
        db.mark_scheduled_failed(id2);
        let all2 = db.list_scheduled_posts(Some(1));
        assert_eq!(
            all2.iter().find(|r| r.0 == id2).map(|r| r.6.as_str()),
            Some("posted")
        );
    }

    #[test]
    fn test_invoice_attempts_bump_and_terminal_state() {
        // The verification-attempts counter advances only via
        // bump_invoice_attempts (the "not settled yet" release path never
        // touches it), and the terminal 'failed' status drops the invoice
        // from pending_invoices() — the verifier's queue.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inv_cap.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.save_invoice(
            "inv1",
            1,
            "5",
            "USDT",
            None,
            "active",
            None,
            Some("gold"),
            Some(7),
            None,
        );
        assert_eq!(db.pending_invoices().len(), 1);
        for expected in 1..=10 {
            assert_eq!(db.bump_invoice_attempts("inv1"), expected);
        }
        // Counter column is stored, not just returned.
        let raw: i64 = {
            let conn = Connection::open(&path).unwrap();
            conn.query_row(
                "SELECT attempts FROM invoices WHERE invoice_id='inv1'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(raw, 10);
        // Terminal-failed leaves the queue (verifier + release paths use the
        // same status vocabulary the rest of the codebase writes).
        db.update_invoice_status("inv1", "failed");
        assert!(db.pending_invoices().is_empty());
        // And an unrelated invoice keeps its own counter at zero.
        db.save_invoice(
            "inv2",
            1,
            "5",
            "USDT",
            None,
            "active",
            None,
            Some("gold"),
            Some(7),
            None,
        );
        assert_eq!(db.bump_invoice_attempts("inv2"), 1);
    }

    #[test]
    fn test_wal_checkpoint_best_effort() {
        // checkpoint() must never panic — including when WAL is unavailable
        // (in-memory dbs) or the file was never written — and must leave the
        // database usable afterwards.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.set_config("k", "v");
        db.checkpoint();
        assert_eq!(db.get_config("k").as_deref(), Some("v"));
        db.checkpoint();
        // On a WAL-mode file the TRUNCATE checkpoint empties the -wal log.
        let wal = dir.path().join("wal.db-wal");
        if wal.exists() {
            assert_eq!(wal.metadata().unwrap().len(), 0);
        }
    }

    #[test]
    fn test_plan_namespace_cross_seller_isolation() {
        // (seller_id, name) namespace: two sellers may both define "pro" and
        // each resolves only their own row; a name unique to another seller
        // is invisible from the wrong namespace. seller_id 0 is the
        // admin/owner global namespace.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ns.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.add_plan(1, "pro", "10", "USDT", 30, None);
        db.add_plan(2, "pro", "20", "USDT", 30, None);
        let p1 = db.find_plans_by_name(Some(1), "pro");
        let p2 = db.find_plans_by_name(Some(2), "pro");
        assert_eq!(p1.len(), 1);
        assert_eq!(p1[0].1, "10");
        assert_eq!(p1[0].6, 1);
        assert_eq!(p2.len(), 1);
        assert_eq!(p2[0].1, "20");
        assert_eq!(p2[0].6, 2);
        // a name unique to seller 2 is not resolvable from seller 1
        db.add_plan(2, "solo", "5", "TON", 7, None);
        assert!(db.find_plans_by_name(Some(1), "solo").is_empty());
        // the global namespace (0) is its own namespace, not a wildcard
        db.add_plan(0, "legacy", "3", "USDT", 30, None);
        assert_eq!(db.find_plans_by_name(Some(0), "legacy").len(), 1);
        assert!(db.find_plans_by_name(Some(1), "legacy").is_empty());
        // cross-seller active view returns all namespaces (caller filters)
        assert_eq!(db.find_plans_by_name(None, "pro").len(), 2);
        // the (seller_id, name) uniqueness holds per seller: re-adding the
        // same name to one seller upserts (INSERT OR REPLACE), it does not
        // create a second row or collide with the other seller's plan.
        db.add_plan(1, "pro", "11", "USDT", 30, None);
        assert_eq!(db.find_plans_by_name(Some(1), "pro").len(), 1);
        assert_eq!(db.find_plans_by_name(Some(1), "pro")[0].1, "11");
    }

    #[test]
    fn test_plan_namespace_dedupe_migration() {
        // Legacy database: plans WITHOUT the seller_id column and WITHOUT the
        // UNIQUE(seller_id, name) constraint (both arrive via migration), so
        // duplicate same-name rows can exist. Db::new must dedupe them
        // (keeping the LOWEST id), re-point subscriptions/activation codes,
        // and create the unique index — while a fresh database must NOT get
        // a redundant manual index next to the schema-level constraint.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE plans (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, price TEXT NOT NULL, asset TEXT NOT NULL, duration_days INTEGER NOT NULL, channel_id TEXT, active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL);
                 CREATE TABLE subscriptions (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER NOT NULL, plan_id INTEGER, channel_id TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'active', starts_at TEXT NOT NULL, expires_at TEXT NOT NULL, invoice_id TEXT, code_id INTEGER, created_at TEXT NOT NULL);
                 CREATE TABLE activation_codes (id INTEGER PRIMARY KEY AUTOINCREMENT, code TEXT NOT NULL UNIQUE, plan_id INTEGER, value_days INTEGER, max_uses INTEGER NOT NULL DEFAULT 1, uses INTEGER NOT NULL DEFAULT 0, expires_at TEXT, created_at TEXT NOT NULL);
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','10','USDT',30,'2024-01-01');
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','20','USDT',30,'2024-01-02');
                 INSERT INTO plans(name,price,asset,duration_days,created_at) VALUES('gold','30','USDT',30,'2024-01-03');
                 INSERT INTO subscriptions(user_id,plan_id,channel_id,starts_at,expires_at,created_at) VALUES(7,3,'@c','2024-01-01','2024-02-01','2024-01-01');
                 INSERT INTO activation_codes(code,plan_id,created_at) VALUES('CODE1',2,'2024-01-01');",
            )
            .unwrap();
        }
        let db = Db::new(path.to_str().unwrap()).unwrap();
        // All three legacy rows carried seller_id 0; the lowest id (price
        // '10') survived.
        let kept = db.get_plan(0, "gold").expect("kept lowest-id row");
        assert_eq!(kept.0, 1);
        assert_eq!(kept.1, "10");
        assert_eq!(db.list_plans(Some(0)).len(), 1);
        // References were re-pointed to the kept row, not orphaned.
        {
            let conn = Connection::open(&path).unwrap();
            let sub_plan: i64 = conn
                .query_row(
                    "SELECT plan_id FROM subscriptions WHERE user_id=7",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(sub_plan, 1);
            let code_plan: i64 = conn
                .query_row(
                    "SELECT plan_id FROM activation_codes WHERE code='CODE1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(code_plan, 1);
            // The legacy table got the unique index (it has no table-level
            // constraint for sqlite to rely on).
            let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE name='idx_plans_seller_name'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(idx, 1);
            // The index actually enforces the namespace now.
            let dup = conn.execute(
                "INSERT INTO plans(seller_id,name,price,asset,duration_days,active,created_at) VALUES(0,'gold','99','USDT',30,1,'2024-01-04')",
                [],
            );
            assert!(dup.is_err(), "unique index must reject a duplicate");
        }
        // A FRESH database relies on the schema-level UNIQUE(seller_id, name)
        // constraint and must not carry the redundant manual index.
        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh = Db::new(fresh_dir.path().join("fresh.db").to_str().unwrap()).unwrap();
        drop(fresh);
        {
            let conn = Connection::open(fresh_dir.path().join("fresh.db")).unwrap();
            let idx: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE name='idx_plans_seller_name'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(
                idx, 0,
                "fresh db uses the schema constraint, not a dup index"
            );
            let constraint: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_index_list('plans') WHERE \"unique\"=1 AND origin='u'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(constraint, 1);
        }
    }

    #[test]
    fn test_channels_add_dedupe_and_scoping() {
        // The channels store: seller_id 0 is the global namespace; names are
        // unique console-wide and case-insensitive (a Telegram @username
        // names one channel no matter who references it).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channels.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        assert!(db.add_channel("@global", 0).is_ok());
        assert!(db.add_channel("@seller1", 1).is_ok());
        // Dedupe: same channel, different case or namespace, is refused with
        // an honest message — not a raw SQLite constraint dump. (The message
        // names the caller's spelling, so assert the refusal class, not an
        // exact string across cases.)
        let dup = db.add_channel("@GLOBAL", 0).unwrap_err();
        assert!(dup.contains("already exists"), "got: {dup}");
        assert!(
            db.add_channel("@global", 1)
                .unwrap_err()
                .contains("already exists"),
            "dedupe is console-wide across namespaces"
        );
        assert_eq!(db.list_channels(None).len(), 2, "duplicates must not land");
        // Empty names are rejected.
        assert!(db.add_channel("   ", 0).is_err());
        // Scoping: the owner view (None) sees everything; a seller sees the
        // global namespace plus their own rows, never another seller's.
        assert_eq!(db.list_channels(None).len(), 2);
        let s1 = db.list_channels(Some(1));
        assert_eq!(s1.len(), 2);
        assert!(s1.iter().any(|c| c.channel == "@global"));
        assert!(s1.iter().any(|c| c.channel == "@seller1"));
        db.add_channel("@seller2", 2).unwrap();
        let s2 = db.list_channels(Some(2));
        assert_eq!(s2.len(), 2);
        assert!(
            s2.iter().all(|c| c.channel != "@seller1"),
            "other sellers invisible"
        );
        // Row shape: named fields, global namespace, not a default.
        let row = db
            .list_channels(None)
            .into_iter()
            .find(|c| c.channel == "@global")
            .unwrap();
        assert_eq!(row.seller_id, 0);
        assert!(!row.is_default);
        assert!(!row.created_at.is_empty());
    }

    #[test]
    fn test_channels_remove_scoping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rm.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        db.add_channel("@global", 0).unwrap();
        db.add_channel("@own", 1).unwrap();
        db.add_channel("@other", 2).unwrap();
        // A seller cannot remove the global row or another seller's row.
        assert!(db.remove_channel("@global", 1).is_err());
        assert!(db.remove_channel("@other", 1).is_err());
        assert!(
            db.remove_channel("@OTHER", 1).is_err(),
            "case-insensitive name, still scoped"
        );
        assert_eq!(
            db.list_channels(None).len(),
            3,
            "nothing deleted by cross-scope attempts"
        );
        // A seller removes their own row.
        assert!(db.remove_channel("@own", 1).is_ok());
        assert!(
            db.remove_channel("@own", 1).is_err(),
            "second remove is not-found"
        );
        // The owner (0) removes global rows only.
        assert!(db.remove_channel("@global", 0).is_ok());
        assert!(
            db.remove_channel("@other", 0).is_err(),
            "owner does not reach seller rows here"
        );
        assert_eq!(
            db.list_channels(None).len(),
            1,
            "only @other (seller 2) survives"
        );
        // Empty names are refused, not silently ignored.
        assert!(db.remove_channel("", 0).is_err());
    }

    #[test]
    fn test_channels_default_switches_exactly_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("default.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        // Missing channels are upserted as new global defaults.
        db.set_default_channel("@a").unwrap();
        assert_eq!(db.default_channel().as_deref(), Some("@a"));
        db.add_channel("@b", 0).unwrap();
        // Switching: exactly one row carries the flag afterwards.
        db.set_default_channel("@b").unwrap();
        assert_eq!(db.default_channel().as_deref(), Some("@b"));
        let defaults = db
            .list_channels(None)
            .into_iter()
            .filter(|c| c.is_default)
            .count();
        assert_eq!(defaults, 1, "switching must clear every other is_default");
        // A seller-owned channel cannot become the global default.
        db.add_channel("@seller", 5).unwrap();
        let err = db.set_default_channel("@seller").unwrap_err();
        assert!(err.contains("belongs to a seller"), "got: {err}");
        assert_eq!(
            db.default_channel().as_deref(),
            Some("@b"),
            "refusal must not clobber"
        );
        // Re-flipping back to a global row works, still exactly one default.
        db.set_default_channel("@a").unwrap();
        assert_eq!(db.default_channel().as_deref(), Some("@a"));
        assert_eq!(
            db.list_channels(None)
                .into_iter()
                .filter(|c| c.is_default)
                .count(),
            1
        );
        // Empty names are rejected.
        assert!(db.set_default_channel("  ").is_err());
    }

    #[test]
    fn test_default_channel_legacy_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy-fallback.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        // Nothing anywhere: None.
        assert!(db.default_channel().is_none());
        // Legacy installs: the free-text `channel` config key is honored.
        db.set_config("channel", "@legacy");
        assert_eq!(db.default_channel().as_deref(), Some("@legacy"));
        // An empty legacy value is never resolved as a channel.
        db.set_config("channel", "");
        assert!(
            db.default_channel().is_none(),
            "empty legacy value is not a channel"
        );
        db.set_config("channel", "@legacy");
        // A table default wins over the legacy key.
        db.set_default_channel("@table").unwrap();
        assert_eq!(db.default_channel().as_deref(), Some("@table"));
        // Seller-owned rows never resolve as the global default, flag or not.
        db.add_channel("@sellerch", 7).unwrap();
        db.set_default_channel("@sellerch").unwrap_err();
        assert_eq!(db.default_channel().as_deref(), Some("@table"));
    }

    #[test]
    fn test_channels_migration_seeds_from_legacy_config() {
        // Legacy database: only the config table with the `channel` key. The
        // one-time migration seeds the channels table with that value as the
        // single default global row.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE config(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO config(key,value,updated_at) VALUES('channel','@old','2024-01-01');",
            )
            .unwrap();
        }
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let rows = db.list_channels(None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].channel, "@old");
        assert_eq!(rows[0].seller_id, 0, "seed lands in the global namespace");
        assert!(rows[0].is_default, "seeded row is the default");
        assert_eq!(db.default_channel().as_deref(), Some("@old"));
        // Reopen (second startup): the config marker prevents a re-seed.
        drop(db);
        let db2 = Db::new(path.to_str().unwrap()).unwrap();
        assert_eq!(db2.list_channels(None).len(), 1);
        // Removing the channel afterwards must not resurrect it on restart.
        assert!(db2.remove_channel("@old", 0).is_ok());
        drop(db2);
        let db3 = Db::new(path.to_str().unwrap()).unwrap();
        assert!(db3.list_channels(None).is_empty());
        // A legacy db with an empty `channel` value seeds nothing.
        let empty_dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(empty_dir.path().join("e.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE config(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);
                 INSERT INTO config(key,value,updated_at) VALUES('channel','','2024-01-01');",
            )
            .unwrap();
        }
        let empty = Db::new(empty_dir.path().join("e.db").to_str().unwrap()).unwrap();
        assert!(empty.list_channels(None).is_empty());
        assert!(empty.default_channel().is_none());
        // A fresh database (no legacy key at all) seeds nothing either.
        let fresh = Db::new(dir.path().join("fresh.db").to_str().unwrap()).unwrap();
        assert!(fresh.list_channels(None).is_empty());
    }

    #[test]
    fn test_claim_code_stable_while_unclaimed() {
        // Fresh install (no owner account, setup not completed): a claim code
        // is generated, stored, and served — and repeated startups (same Db
        // and a reopened Db) keep the SAME code while it stays unclaimed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claim.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        assert!(!db.owner_account_exists());
        let first = db.ensure_claim_code().expect("code must be generated");
        assert_eq!(first.len(), 12, "gen_token(12) shape");
        assert_eq!(db.claim_code().as_deref(), Some(first.as_str()));
        assert_eq!(
            db.ensure_claim_code().as_deref(),
            Some(first.as_str()),
            "not regenerated within one run"
        );
        drop(db);
        let db2 = Db::new(path.to_str().unwrap()).unwrap();
        assert_eq!(
            db2.ensure_claim_code().as_deref(),
            Some(first.as_str()),
            "not regenerated across startups"
        );
    }

    #[tokio::test]
    async fn test_claim_code_cleared_when_owner_exists() {
        // Once an owner account exists, the stored code is stale: startup
        // maintenance clears it and claim mode can never return.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claim-owner.db");
        let db = Db::new(path.to_str().unwrap()).unwrap();
        let code = db.ensure_claim_code().expect("pre-claim code exists");
        db.create_web_account("owner", "password123", "owner", None)
            .await
            .expect("owner account created");
        assert!(db.owner_account_exists());
        assert!(
            db.ensure_claim_code().is_none(),
            "no code printed once claimed"
        );
        assert!(
            db.claim_code().is_none(),
            "stale code cleared from the store"
        );
        assert_ne!(db.claim_code().as_deref(), Some(code.as_str()));
        // Repeated startups stay clean (no resurrection, no churn).
        drop(db);
        let db2 = Db::new(path.to_str().unwrap()).unwrap();
        assert!(db2.ensure_claim_code().is_none());
    }

    proptest! {
        #[test]
        fn prop_plan_create_list(seller_id in 1i64..101i64, name in "[a-z]{3,8}") {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("plan.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            db.add_plan(seller_id, &name, "1", "USDT", 7, None);
            let plans = db.list_plans(Some(seller_id));
            prop_assert!(!plans.is_empty());
        }

        #[test]
        fn prop_code_uniqueness(_n in 0u8..1u8) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("code.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            db.add_plan(1, "P", "1", "USDT", 7, None);
            let pid = db.get_plan(1, "P").unwrap().0;
            let mut codes = std::collections::HashSet::new();
            for _ in 0..10 {
                let c = db.create_code(1, Some(pid), Some(7), 1, None, 12);
                codes.insert(c);
            }
            prop_assert_eq!(codes.len(), 10);
        }

        #[test]
        fn prop_subscription_roundtrip(seller_id in 1i64..101i64, user_id in 1i64..1001i64) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("sub.db");
            let db = Db::new(path.to_str().unwrap()).unwrap();
            let id = db.create_subscription(seller_id, user_id, None, "@chan", 7, None, None);
            prop_assert!(id > 0);
            prop_assert!(db.user_has_active_sub(seller_id, user_id, "@chan"));
        }
    }
}
