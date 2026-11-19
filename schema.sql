-- Sellers (multi-tenant)
CREATE TABLE IF NOT EXISTS sellers (
    user_id        INTEGER PRIMARY KEY,
    username       TEXT,
    default_channel TEXT,
    created_at     TEXT NOT NULL
);

-- Target channel configuration (global default)
CREATE TABLE IF NOT EXISTS config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- Posted messages log
CREATE TABLE IF NOT EXISTS posts (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    chat_id     TEXT NOT NULL,
    message_id  INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    content     TEXT,
    created_at  TEXT NOT NULL
);

-- Crypto Pay invoices
CREATE TABLE IF NOT EXISTS invoices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    invoice_id   TEXT NOT NULL UNIQUE,
    seller_id    INTEGER NOT NULL,
    amount       TEXT NOT NULL,
    asset        TEXT NOT NULL,
    description  TEXT,
    status       TEXT NOT NULL DEFAULT 'active',
    pay_url      TEXT,
    plan_id      TEXT,
    user_id      INTEGER,
    attempts     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- Admin allow-list (superadmin always admin)
CREATE TABLE IF NOT EXISTS admins (
    user_id    INTEGER PRIMARY KEY,
    username   TEXT,
    added_at   TEXT NOT NULL
);

-- Subscription plans (per seller)
CREATE TABLE IF NOT EXISTS plans (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id     INTEGER NOT NULL,
    name          TEXT NOT NULL,
    price         TEXT NOT NULL,
    asset         TEXT NOT NULL,
    duration_days INTEGER NOT NULL,
    channel_id    TEXT,
    active        INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL,
    UNIQUE(seller_id, name)
);

-- Subscriptions / access grants
CREATE TABLE IF NOT EXISTS subscriptions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    plan_id     INTEGER,
    channel_id  TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'active',
    starts_at   TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    invoice_id  TEXT,
    code_id     INTEGER,
    created_at  TEXT NOT NULL
);

-- Digital products (per seller; /buy multiplexes these with plans by name)
CREATE TABLE IF NOT EXISTS products (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    name        TEXT NOT NULL,
    price       TEXT NOT NULL,
    asset       TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('digital_link','digital_file','physical')),
    delivery    TEXT,
    stock       INTEGER,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    UNIQUE(seller_id, name)
);

-- Lifetime product grants (one grant per invoice)
CREATE TABLE IF NOT EXISTS product_grants (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    product_id  INTEGER NOT NULL,
    invoice_id  TEXT NOT NULL UNIQUE,
    status      TEXT NOT NULL DEFAULT 'active',
    created_at  TEXT NOT NULL
);

-- Orders for physical products (one per invoice; v1.3-lite)
CREATE TABLE IF NOT EXISTS orders (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    order_id       TEXT NOT NULL UNIQUE,
    seller_id      INTEGER NOT NULL,
    product_id     INTEGER NOT NULL,
    buyer_user_id  INTEGER NOT NULL,
    invoice_id     TEXT NOT NULL UNIQUE,
    status         TEXT NOT NULL DEFAULT 'pending',
    tracking       TEXT,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL
);

-- Activation codes (per seller)
CREATE TABLE IF NOT EXISTS activation_codes (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    code        TEXT NOT NULL UNIQUE,
    plan_id     INTEGER,
    value_days  INTEGER,
    max_uses    INTEGER NOT NULL DEFAULT 1,
    uses        INTEGER NOT NULL DEFAULT 0,
    expires_at  TEXT,
    revoked     INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL
);

-- Code redemptions
CREATE TABLE IF NOT EXISTS code_redemptions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    code_id     INTEGER NOT NULL,
    user_id     INTEGER NOT NULL,
    redeemed_at TEXT NOT NULL
);

-- Users (for public mode + broadcasting, per seller optional)
CREATE TABLE IF NOT EXISTS users (
    user_id    INTEGER PRIMARY KEY,
    username   TEXT,
    first_seen TEXT NOT NULL,
    last_seen  TEXT NOT NULL
);
-- AI templates (per seller)
CREATE TABLE IF NOT EXISTS ai_templates (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id     INTEGER NOT NULL,
    name          TEXT NOT NULL,
    description   TEXT,
    system_prompt TEXT,
    user_prompt   TEXT NOT NULL,
    variables     TEXT,
    active        INTEGER NOT NULL DEFAULT 1,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    UNIQUE(seller_id, name)
);

-- Web dashboard accounts (per seller/admin/owner)
CREATE TABLE IF NOT EXISTS web_accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    salt          TEXT NOT NULL,
    role          TEXT NOT NULL,
    seller_id     INTEGER,
    totp_secret   TEXT,
    totp_enabled  INTEGER NOT NULL DEFAULT 0,
    totp_last_used INTEGER,
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS web_pending_mfa (
    token_hash  TEXT PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    purpose     TEXT NOT NULL,
    totp_secret TEXT,
    expires_at  TEXT NOT NULL,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS web_recovery_codes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id INTEGER NOT NULL,
    code_hash  TEXT NOT NULL,
    used       INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

-- Web dashboard sessions
CREATE TABLE IF NOT EXISTS web_sessions (
    token_hash  TEXT PRIMARY KEY,
    account_id  INTEGER NOT NULL,
    role        TEXT NOT NULL,
    seller_id   INTEGER,
    expires_at  TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS ai_usage (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id         INTEGER NOT NULL,
    user_id           INTEGER,
    provider          TEXT NOT NULL,
    model             TEXT NOT NULL,
    template          TEXT,
    prompt_tokens     INTEGER NOT NULL DEFAULT 0,
    completion_tokens INTEGER NOT NULL DEFAULT 0,
    total_tokens      INTEGER NOT NULL DEFAULT 0,
    estimated_cost    REAL NOT NULL DEFAULT 0,
    status            TEXT NOT NULL,
    error_message     TEXT,
    request_id        TEXT,
    created_at        TEXT NOT NULL
);

-- Refunds
CREATE TABLE IF NOT EXISTS refunds (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    invoice_id  TEXT,
    user_id     INTEGER,
    amount      TEXT NOT NULL,
    asset       TEXT NOT NULL,
    reason      TEXT,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- Payouts
CREATE TABLE IF NOT EXISTS payouts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id    INTEGER NOT NULL,
    amount       TEXT NOT NULL,
    asset        TEXT NOT NULL,
    destination  TEXT,
    status       TEXT NOT NULL DEFAULT 'pending',
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);

-- Audit log
CREATE TABLE IF NOT EXISTS audit_log (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    actor_id     INTEGER NOT NULL,
    actor_role   TEXT NOT NULL,
    action       TEXT NOT NULL,
    target_type  TEXT NOT NULL,
    target_id    TEXT,
    metadata     TEXT,
    created_at   TEXT NOT NULL
);

-- API keys
CREATE TABLE IF NOT EXISTS api_keys (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id   INTEGER NOT NULL,
    name         TEXT,
    key_hash     TEXT NOT NULL UNIQUE,
    key_prefix   TEXT,
    scopes       TEXT,
    revoked      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    expires_at   TEXT,
    last_used_at TEXT
);

-- Scheduled posts
CREATE TABLE IF NOT EXISTS scheduled_posts (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id        INTEGER NOT NULL,
    chat_id          TEXT NOT NULL,
    content          TEXT,
    kind             TEXT NOT NULL,
    scheduled_at     TEXT NOT NULL,
    status           TEXT NOT NULL DEFAULT 'pending',
    created_at       TEXT NOT NULL,
    posted_message_id INTEGER,
    attempts         INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_scheduled_posts_status_time ON scheduled_posts(status, scheduled_at);

-- Managed channels (v1.3.x: "channels become managed data"). seller_id 0 is
-- the global namespace (same convention as the plan namespace); owner-managed
-- global channels and seller-owned ones live in one table. Channel names are
-- unique console-wide and case-insensitive: a Telegram @username names one
-- channel no matter who references it. Exactly one row may carry is_default.
CREATE TABLE IF NOT EXISTS channels (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    channel         TEXT NOT NULL UNIQUE COLLATE NOCASE,
    title           TEXT,
    seller_id       INTEGER NOT NULL DEFAULT 0,
    is_default      INTEGER NOT NULL DEFAULT 0,
    sender_identity TEXT NOT NULL DEFAULT 'bot',
    created_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_channels_seller ON channels(seller_id);

-- Phase 1: promo codes + shopping cart
CREATE TABLE IF NOT EXISTS promo_codes (
    code        TEXT NOT NULL,
    seller_id   INTEGER NOT NULL DEFAULT 0,
    promo_type  TEXT NOT NULL CHECK (promo_type IN ('percent','fixed')),
    value       REAL NOT NULL,
    currency    TEXT,
    max_uses    INTEGER,
    uses        INTEGER NOT NULL DEFAULT 0,
    min_purchase REAL NOT NULL DEFAULT 0,
    scope       TEXT NOT NULL DEFAULT 'all',
    scope_id    INTEGER,
    expires_at  TEXT,
    active      INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (code, seller_id)
);
CREATE TABLE IF NOT EXISTS promo_redemptions (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    code           TEXT NOT NULL,
    seller_id      INTEGER NOT NULL,
    buyer_id       INTEGER NOT NULL,
    invoice_id     TEXT,
    discount_amount REAL NOT NULL DEFAULT 0,
    created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_promo_redemptions_seller ON promo_redemptions(seller_id);
CREATE TABLE IF NOT EXISTS cart_items (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    buyer_id   INTEGER NOT NULL,
    seller_id  INTEGER NOT NULL DEFAULT 0,
    item_type  TEXT NOT NULL CHECK (item_type IN ('plan','product')),
    item_id    INTEGER NOT NULL,
    qty        INTEGER NOT NULL DEFAULT 1,
    added_at   TEXT NOT NULL,
    UNIQUE (buyer_id, seller_id, item_type, item_id)
);
CREATE INDEX IF NOT EXISTS idx_cart_items_buyer ON cart_items(buyer_id, seller_id);

-- Phase 2: referrals + bonus/loyalty balance
CREATE TABLE IF NOT EXISTS referrals (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id    INTEGER NOT NULL DEFAULT 0,
    referrer_id  INTEGER NOT NULL,
    referee_id   INTEGER NOT NULL,
    code         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending',
    reward_type  TEXT,
    reward_value REAL NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    rewarded_at  TEXT,
    UNIQUE (seller_id, referee_id)
);
CREATE INDEX IF NOT EXISTS idx_referrals_seller ON referrals(seller_id);
CREATE INDEX IF NOT EXISTS idx_referrals_referrer ON referrals(seller_id, referrer_id);
CREATE TABLE IF NOT EXISTS referral_codes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id  INTEGER NOT NULL DEFAULT 0,
    user_id    INTEGER NOT NULL,
    code       TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (seller_id, user_id),
    UNIQUE (seller_id, code)
);
CREATE TABLE IF NOT EXISTS bonus_balances (
    user_id      INTEGER NOT NULL,
    seller_id    INTEGER NOT NULL DEFAULT 0,
    balance_minor INTEGER NOT NULL DEFAULT 0,
    updated_at   TEXT NOT NULL,
    PRIMARY KEY (user_id, seller_id)
);
CREATE TABLE IF NOT EXISTS bonus_ledger (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id      INTEGER NOT NULL,
    seller_id    INTEGER NOT NULL DEFAULT 0,
    amount_minor INTEGER NOT NULL,
    reason       TEXT,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_bonus_ledger_user ON bonus_ledger(user_id, seller_id);

-- MCP approvals: sensitive tool calls park here until the owner decides via
-- the bot; args_json is replayed verbatim on approval.
CREATE TABLE IF NOT EXISTS mcp_approvals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    tool        TEXT NOT NULL,
    args_json   TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL,
    expires_at  TEXT NOT NULL,
    decided_at  TEXT,
    executed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_mcp_approvals_status ON mcp_approvals(status);

-- MCP audit: one row per tool-call outcome (ok or err), detail truncated by
-- the insert helper so secrets never land here. seller_id records which
-- seller principal ran the tool (v1.4 D6); owner rows stay NULL.
CREATE TABLE IF NOT EXISTS mcp_audit (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tool       TEXT NOT NULL,
    role       TEXT NOT NULL,
    ok         INTEGER NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL,
    seller_id  INTEGER
);
CREATE INDEX IF NOT EXISTS idx_mcp_audit_created ON mcp_audit(created_at);

-- Per-seller MCP tokens (v2): bearer tokens scoped to a seller account.
-- Not yet wired into auth (MCP_OWNER_TOKEN remains the only live credential);
-- reserved for the v2 per-seller MCP token flow.
CREATE TABLE IF NOT EXISTS mcp_tokens (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id    INTEGER NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    token_prefix TEXT,
    name         TEXT,
    revoked      INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    expires_at   TEXT,
    last_used_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_mcp_tokens_hash ON mcp_tokens(token_hash);
CREATE INDEX IF NOT EXISTS idx_mcp_tokens_seller ON mcp_tokens(seller_id);

-- Multi-bot (v1.6): one row per configured bot. `name` is the console
-- display name (v1.7); '' falls back to the key. `bot_type` (multibot
-- foundation) is the product/capability type of the bot; the DEFAULT
-- backfills every legacy row to 'shop_bot' with no destructive rewrite.
-- Declared last so legacy (ALTERed) and fresh layouts match.
CREATE TABLE IF NOT EXISTS bots (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    key               TEXT NOT NULL UNIQUE,
    username          TEXT,
    owner_user_id     INTEGER NOT NULL,
    default_channel_id INTEGER,
    enabled           INTEGER NOT NULL DEFAULT 1,
    created_at        TEXT NOT NULL,
    name              TEXT NOT NULL DEFAULT '',
    bot_type          TEXT NOT NULL DEFAULT 'shop_bot'
);
CREATE INDEX IF NOT EXISTS idx_bots_owner ON bots(owner_user_id);
-- idx_bots_type is created in Db::migrate, after the additive bot_type
-- ALTER: a legacy bots table has no such column when schema.sql runs.

-- AI-assisted post drafts
CREATE TABLE IF NOT EXISTS ai_post_drafts (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id           INTEGER NOT NULL,
    channel             TEXT NOT NULL,
    template            TEXT,
    content             TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'draft',
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL,
    published_message_id INTEGER
);
CREATE INDEX IF NOT EXISTS idx_ai_post_drafts_seller ON ai_post_drafts(seller_id);

-- Bookings (v2): services, slots, bookings. Reuses invoice pipeline optionally.
CREATE TABLE IF NOT EXISTS services (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id       INTEGER NOT NULL,
    name            TEXT NOT NULL,
    price           TEXT NOT NULL,
    asset           TEXT NOT NULL,
    duration_minutes INTEGER NOT NULL DEFAULT 60,
    channel_id      TEXT,
    active          INTEGER NOT NULL DEFAULT 1,
    created_at      TEXT NOT NULL,
    UNIQUE(seller_id, name)
);
CREATE INDEX IF NOT EXISTS idx_services_seller ON services(seller_id);

CREATE TABLE IF NOT EXISTS slots (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    service_id  INTEGER NOT NULL,
    seller_id   INTEGER NOT NULL,
    starts_at   TEXT NOT NULL,
    ends_at     TEXT,
    capacity    INTEGER NOT NULL DEFAULT 1,
    booked      INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'available',
    held_until  TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slots_service ON slots(service_id);
CREATE INDEX IF NOT EXISTS idx_slots_status ON slots(status);

CREATE TABLE IF NOT EXISTS bookings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    booking_id  TEXT NOT NULL UNIQUE,
    seller_id   INTEGER NOT NULL,
    service_id  INTEGER NOT NULL,
    slot_id     INTEGER,
    user_id     INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'held',
    invoice_id  TEXT,
    expires_at  TEXT,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_bookings_seller ON bookings(seller_id);
CREATE INDEX IF NOT EXISTS idx_bookings_status ON bookings(status);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_invoices_status ON invoices(status);
CREATE INDEX IF NOT EXISTS idx_subscriptions_status_expires ON subscriptions(status, expires_at);
CREATE INDEX IF NOT EXISTS idx_products_seller ON products(seller_id);
CREATE INDEX IF NOT EXISTS idx_product_grants_seller ON product_grants(seller_id);
CREATE INDEX IF NOT EXISTS idx_orders_seller_status ON orders(seller_id, status);
CREATE INDEX IF NOT EXISTS idx_refunds_seller ON refunds(seller_id);
CREATE INDEX IF NOT EXISTS idx_payouts_seller ON payouts(seller_id);
CREATE INDEX IF NOT EXISTS idx_audit_actor ON audit_log(actor_id);
CREATE INDEX IF NOT EXISTS idx_audit_created ON audit_log(created_at);
CREATE INDEX IF NOT EXISTS idx_api_keys_account ON api_keys(account_id);
CREATE INDEX IF NOT EXISTS idx_api_keys_hash ON api_keys(key_hash);

-- Phase 3: CRM + support tickets
CREATE TABLE IF NOT EXISTS customers (
    user_id     INTEGER NOT NULL,
    seller_id   INTEGER NOT NULL DEFAULT 0,
    first_seen  TEXT NOT NULL,
    last_seen   TEXT NOT NULL,
    tags        TEXT,
    notes       TEXT,
    total_spent REAL NOT NULL DEFAULT 0,
    order_count INTEGER NOT NULL DEFAULT 0,
    status      TEXT NOT NULL DEFAULT 'active',
    PRIMARY KEY (user_id, seller_id)
);
CREATE INDEX IF NOT EXISTS idx_customers_seller ON customers(seller_id);
CREATE TABLE IF NOT EXISTS tickets (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id  INTEGER NOT NULL DEFAULT 0,
    buyer_id   INTEGER NOT NULL,
    subject    TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'open',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tickets_seller ON tickets(seller_id);
CREATE INDEX IF NOT EXISTS idx_tickets_buyer ON tickets(buyer_id);
CREATE TABLE IF NOT EXISTS ticket_messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id   INTEGER NOT NULL,
    sender_role TEXT NOT NULL,
    sender_id   INTEGER NOT NULL,
    text        TEXT NOT NULL,
    created_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ticket_messages_ticket ON ticket_messages(ticket_id);

-- Phase 4: recovery & engagement
CREATE TABLE IF NOT EXISTS cart_abandonment (
    buyer_id       INTEGER NOT NULL,
    seller_id      INTEGER NOT NULL DEFAULT 0,
    abandoned_at   TEXT NOT NULL,
    reminded_count INTEGER NOT NULL DEFAULT 0,
    reminded_at    TEXT,
    recovered_at   TEXT,
    PRIMARY KEY (buyer_id, seller_id)
);
CREATE TABLE IF NOT EXISTS reorder_schedules (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    buyer_id        INTEGER NOT NULL,
    seller_id       INTEGER NOT NULL DEFAULT 0,
    product_id      INTEGER NOT NULL,
    interval_days   INTEGER NOT NULL,
    last_ordered_at TEXT,
    next_reminder_at TEXT,
    enabled         INTEGER NOT NULL DEFAULT 1,
    UNIQUE (buyer_id, seller_id, product_id)
);
CREATE TABLE IF NOT EXISTS reviews (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    buyer_id   INTEGER NOT NULL,
    seller_id  INTEGER NOT NULL DEFAULT 0,
    product_id INTEGER NOT NULL,
    rating     INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    text       TEXT,
    approved   INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    UNIQUE (buyer_id, product_id)
);
CREATE INDEX IF NOT EXISTS idx_reviews_product ON reviews(seller_id, product_id);

-- ===========================================================================
-- Cross-cutting moderation (multibot foundation, Step 4 storage).
--
-- Every record is scoped by seller_id (the house tenant key, intentionally
-- WITHOUT an FK so callers may scope writes before a seller row exists —
-- this matches posts/plans/invoices). Per-bot overrides are scoped by BOTH
-- seller_id and bot_id. bot_id carries a real FK to bots: deleting a bot
-- cascades its local overrides and nulls the optional reference on appeals
-- and decisions so the append-only audit trail survives bot deletion.
--
-- PRIVACY CONTRACT (L10 floor): these tables hold policy TERMS and OPAQUE
-- references only. There is deliberately NO message-content column.
-- `message_ref` is an opaque caller token (e.g. "c:123/m:456"); `reason` is
-- the short appeal justification / decision code, never the filtered body.
-- Length caps are enforced in the Db write methods as a second line of
-- defence. Do not add a content column here without an L10 review.
-- ===========================================================================

-- (a) Owner-enforced GLOBAL content-policy floor. Unlike every other table in
-- this section there is deliberately NO seller_id: these terms apply to every
-- seller and bot, and can never be removed or weakened by any seller's
-- allow-list or per-bot override. Writes go only through the explicit
-- `add_moderation_global_floor_term` / `remove_moderation_global_floor_term`
-- Db methods, which the web layer MUST gate to the owner. Seeded empty for
-- backward compatibility — the compile-time constants in db.rs are only a
-- fallback when this table is unreadable. Keys/terms only, no message content.
CREATE TABLE IF NOT EXISTS moderation_global_floor (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    term        TEXT NOT NULL UNIQUE,
    mode        TEXT NOT NULL CHECK (mode IN ('hide','warn','review')),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

-- (b) Seller-wide "Blocked words". One row per (seller, term); mode is the
-- plain-language hide / warn / review action. `enabled` lets the console keep
-- a word without enforcing it (inline status toggle), additive to the floor.
CREATE TABLE IF NOT EXISTS moderation_blocked_words (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    term        TEXT NOT NULL,
    mode        TEXT NOT NULL DEFAULT 'warn' CHECK (mode IN ('hide','warn','review')),
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (seller_id, term)
);
CREATE INDEX IF NOT EXISTS idx_mod_blocked_seller ON moderation_blocked_words(seller_id);

-- (b) "Always allow" list: seller-scoped exemptions that take precedence over
-- that seller's Blocked words, but never over the owner floor terms.
CREATE TABLE IF NOT EXISTS moderation_allow_words (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    term        TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    UNIQUE (seller_id, term)
);
CREATE INDEX IF NOT EXISTS idx_mod_allow_seller ON moderation_allow_words(seller_id);

-- (c) Per-bot override. One sentinel row (term = '', mode = NULL) per
-- (seller_id, bot_id) holds the inheritance flag: 1 = "Using your shop's
-- rules" (default), 0 = "Custom rules for this bot". Custom-term rows (term
-- <> '') carry stricter local rules and are ignored while inheriting. The
-- CHECK keeps the two row shapes disjoint; UNIQUE is the idempotency guard.
CREATE TABLE IF NOT EXISTS moderation_bot_overrides (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id   INTEGER NOT NULL,
    bot_id      INTEGER NOT NULL REFERENCES bots(id) ON DELETE CASCADE,
    term        TEXT NOT NULL DEFAULT '',
    mode        TEXT CHECK (mode IN ('hide','warn','review')),
    inherits    INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    UNIQUE (seller_id, bot_id, term),
    CHECK ((term = '' AND mode IS NULL) OR (term <> '' AND mode IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_mod_bot_override_scope ON moderation_bot_overrides(seller_id, bot_id);

-- Append-only audit of every moderation decision. `term` is the matched
-- POLICY term, not the message; `message_ref` is opaque. No content column.
CREATE TABLE IF NOT EXISTS moderation_decisions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id    INTEGER NOT NULL,
    bot_id       INTEGER REFERENCES bots(id) ON DELETE SET NULL,
    term         TEXT,
    action       TEXT NOT NULL,
    reason       TEXT,
    message_ref  TEXT,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mod_decisions_seller ON moderation_decisions(seller_id, created_at);
CREATE INDEX IF NOT EXISTS idx_mod_decisions_bot ON moderation_decisions(seller_id, bot_id);

-- Appeal review queue for the console (end-user conversation is bot-side,
-- L8). `decision_id` is an opaque link back to the audited decision.
CREATE TABLE IF NOT EXISTS moderation_appeals (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    seller_id    INTEGER NOT NULL,
    bot_id       INTEGER REFERENCES bots(id) ON DELETE SET NULL,
    decision_id  INTEGER REFERENCES moderation_decisions(id) ON DELETE SET NULL,
    message_ref  TEXT NOT NULL,
    reason       TEXT,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','upheld')),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_mod_appeals_seller ON moderation_appeals(seller_id, status);
CREATE INDEX IF NOT EXISTS idx_mod_appeals_bot ON moderation_appeals(seller_id, bot_id);

-- Appeal dedup is enforced by the DB, not by the handler's check-then-insert
-- (L10 LOW: concurrent double-submissions could create duplicate rows).
-- COALESCE(bot_id, 0) folds the bot-less queue into the same key; bot ids are
-- AUTOINCREMENT so 0 is never a real bot. The cleanup MUST precede the index
-- creation: init_schema() executes this file before Db::migrate(), so an
-- upgrade of a legacy DB that already holds duplicate (seller, bot, ref)
-- tuples would otherwise fail to open. Keep the oldest row per tuple (lowest
-- id); distinct tuples are untouched. Idempotent on every subsequent open.
DELETE FROM moderation_appeals
WHERE id IN (
    SELECT d.id FROM moderation_appeals AS d
    WHERE EXISTS (
        SELECT 1 FROM moderation_appeals AS k
        WHERE k.seller_id = d.seller_id
          AND COALESCE(k.bot_id, 0) = COALESCE(d.bot_id, 0)
          AND k.message_ref = d.message_ref
          AND k.id < d.id
    )
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_mod_appeals_dedup
    ON moderation_appeals(seller_id, COALESCE(bot_id, 0), message_ref);
