# Architecture — v1.4 "Close the loop"

Status: ratified plan (architecture, 2026-10-27). Implementers: core → server → design → web, gated
by verification, floored by security; documentation tracked throughout. Companion:
[`ARCHITECTURE.md`](../ARCHITECTURE.md).

v1.4 closes the last open business loop (bookings) and two auth-hygiene debts
(per-seller MCP tokens, plaintext setup passwords). Every claim was verified
against the tree on 2026-10-27; file:line references are current.

## 0. Scope ratification — what changed vs. the proposal

The supervisor's proposal is ratified with four adjustments forced by the code:

1. **The `/book` race is worse than stated.** The availability check lives in
   the handler (`commands.rs:1715-1716`), the hold write in a separate `Db`
   call (`db.rs:5052-5078`) — two concurrent `/book` both pass. Claiming moves
   into the `Db` method as a transaction (D4).
2. **`slots.starts_at` is free text; `slots.ends_at` is never written**
   (`db.rs:5013-5030`) — no page can sort or render slots. v1.4 validates the
   timestamp and derives `ends_at` from `services.duration_minutes` (D4).
3. **The hold changes meaning.** With money deferred (D2), the 15-minute hold
   (`commands.rs:1718`) becomes the seller-response window — 60 minutes, with
   late-confirm resurrection (D3).
4. **Console `/bookings` ships; console services CRUD does not** — responding
   to holds is the urgent surface; CRUD duplicates working bot flows (D8).

## 1. Decision record (ADR summary)

| # | Decision | Rationale (one line) |
|---|----------|----------------------|
| D1 | **Appointment model**: 1 slot = 1 booking for v1.4; class capacity (N seats) deferred to v1.5 | The code already behaves as appointments (whole-slot hold, `db.rs:5071-5076`); class capacity needs atomic seat accounting with no demand signal yet |
| D2 | **Money: free-confirmation-only in v1.4.** Priced bookings (Crypto Pay invoice per booking) deferred to v1.5, pending the finance ruling (Q1) | Booking money semantics (pay-then-hold vs hold-then-pay, expiry-after-payment) are unresolved and would block the milestone on one decision; deferral costs nothing (`bookings.invoice_id` already exists, `schema.sql:397`) |
| D3 | Hold = seller-response window: `expires_at = min(now + 60 min, slot start)`; `/bookingconfirm` may resurrect an expired hold if the slot is still free | One named constant, one rule; covers the late-responding seller without unbounded zombie holds |
| D4 | Slot claim moves into `Db::create_booking` as one transaction; `create_slot` validates time and derives `ends_at` | Kills the handler-level TOCTOU (0.1) and makes slots sortable/renderable |
| D5 | Held-slot sweeper (`main.rs:716-729`) DMs each buyer whose hold expired, mirroring the subscription sweeper (`main.rs:686-714`) | Closes the loop for the buyer; send failure logged, never propagated |
| D6 | **MCP per-seller tokens wired**: owner env-token keeps priority; on mismatch, SHA-256 hash lookup → `Principal{Seller, Some(seller_id)}`; identical 401 for unknown/revoked/expired | Tools already scope by `principal.seller_id` (`tools.rs:447-496`), so wiring is purely additive; no oracle in the failure path |
| D7 | **Setup links replace DM'd passwords**: reuse `web_pending_mfa` (`purpose='setup_link'`), 15-min TTL, `gen_token(48)` hashed, atomic consume-on-POST, auto-login on success | The table is already the pending-token shape (`schema.sql:178-185`); no new table, and no password ever crosses a DM |
| D8 | Console `/bookings`: bookings table + confirm/decline/cancel + read-only services panel; services CRUD stays bot-only (v1.5) | Booking response is the time-critical seller action; CRUD duplication buys nothing this milestone |
| D9 | Trust pack: `CHANGELOG.md` (Keep a Changelog), `docs/RELEASING.md`, Cargo 0.N.0 tracks milestone vN, 1.0 gates defined | Process, kept short (§5) |

## 2. Area A — Bookings v1.4

### D1: appointment model

v1.4 standardizes on **one slot holds exactly one booking**. Slot state is the
only bookability state: `available → held → booked`, back to `available` on
cancel/decline/expiry. Consequences:

- `slots.booked` becomes **dead**: never read for availability, never
  written; the column stays (SQLite drops need a table rebuild); availability
  consults `status` only.
- `/slotadd` keeps its `capacity` argument for CLI stability but clamps it to
  1 and says so: `capacity > 1 is reserved for class bookings (v1.5); slot
  created with capacity 1.` Existing `capacity > 1` rows are harmless
  (capacity is no longer consulted).

Class capacity (N seats, atomic `booked` counter, per-seat holds) is the
explicit v1.5 candidate — **not** half-built here.

### D2: the money story — two options, recommendation, and the finance question

**Option A — priced bookings in v1.4.** `/book` creates a Crypto Pay invoice
bound to the booking, reusing the poller's claim → verify → grant machinery
(`main.rs:343-505`). Schema precedent exists: `invoices.product_id` was added
additively (`db.rs:374-385`); a booking path would add `invoices.booking_id`
the same way, `pending_invoices()` (`db.rs:1381-1406`) would queue on it, and
`bookings.invoice_id` (`schema.sql:397`) already exists for the back-reference.

**Option B — free-confirmation-only in v1.4 (recommended).** `services.price`
stays display-only; the buyer reserves time, settling with the seller directly
(or out-of-band link). The invoice integration is specified at contract level
(Option A) but implemented in v1.5 after the finance ruling.

Why B: the poller's grant dispatch is guarded by subtle idempotency invariants
(claim/`release_invoice_claim`/`finalize_invoice_paid`, `db.rs:1298-1345`);
a third grant arm mid-milestone expands the core/server impact, and the real
blockers are money questions only finance can answer (Q1). B forecloses nothing —
the state machine and `invoice_id` column carry forward unchanged.

**Q1 for finance (rules before the v1.5 spec):**
1. A buyer pays for a booking that then expires unconfirmed or whose slot
   passes without seller confirmation — refund, credit, or rebook? (Decides
   hold-then-pay vs pay-then-hold for the v1.5 invoice.)
2. Is the booking price the service price at `/book` time, or snapshotted
   and refundable per the refunds table (`schema.sql:222-233`)?

### State machine

```
        /book (atomic claim)      /bookingconfirm (seller/admin)
(none) ─────────────────▶ held ──────────────────────────▶ confirmed
                           │ ▲                               │
  sweeper: past expires_at │ │ resurrect: confirm expired    │ /cancelbooking (buyer)
  → expired + free slot    │ │ hold while slot 'available'   │ /bookings/cancel (console)
                           ▼ │                               ▼
                        expired                           cancelled
/bookingdecline: held|expired → declined, slot freed
```

Terminal: `confirmed`, `cancelled`, `declined`; `expired` resurrectable (D3)
only while the slot is `available`.

### Module boundaries

| Artifact | File(s) | Team |
|----------|---------|------|
| Slot/booking state machine, sweep query, `SlotRow` | `src/db.rs` | core |
| `/book` `/slotadd` `/mybookings` `/cancelbooking` `/bookingconfirm` `/bookingdecline` + registry entries; sweeper DM loop (`src/main.rs`) | `src/handlers/commands.rs`, `src/handlers/registry.rs` | server |
| `/bookings` page + booking POST routes | `src/web.rs` | web (spec: design) |

### Interface contracts

**Db methods (exact signatures):**

```rust
pub struct SlotRow { pub id: i64, pub starts_at: String,
                     pub ends_at: Option<String>, pub status: String }

pub fn create_slot(&self, service_id: i64, seller_id: i64, starts_at: &str,
                   capacity: i64) -> Result<i64, String>
// CHANGED (was `-> i64`, db.rs:5013). Validates starts_at parses as RFC3339 or
// "YYYY-MM-DD HH:MM" (UTC) and is future; clamps capacity to 1 (D1); writes
// ends_at = starts_at + services.duration_minutes.

pub fn list_slots(&self, service_id: i64) -> Vec<SlotRow>
// CHANGED (was 5-tuple incl. capacity/booked, db.rs:5032). Status-only view.

pub fn create_booking(&self, seller_id: i64, service_id: i64, slot_id: i64,
                      user_id: i64) -> Result<String, String>
// CHANGED (db.rs:5052): slot_id required (D1); invoice_id/expires_at params
// removed (priced holds are v1.5). ONE transaction: slot starts_at > now;
// UPDATE slots SET status='held', held_until=?exp WHERE id=? AND status='available'
// — 0 rows => Err("slot no longer available") (atomic claim, D4); INSERT
// bookings with expires_at = min(now+BOOKING_HOLD_MINUTES, slot.starts_at);
// BOOKING_HOLD_MINUTES = 60 is the single tuning point (D3).

pub fn confirm_booking(&self, seller_scope: Option<i64>, booking_id: &str)
    -> Result<(i64, i64), String>
pub fn decline_booking(&self, seller_scope: Option<i64>, booking_id: &str)
    -> Result<(i64, i64), String>
// seller_scope Some(sid) = seller's own rows only; None = admin/owner.
// confirm: held|expired (expired only while slot 'available' — re-holds then
// books in the same transaction); decline: held|expired, frees the slot.
// Returns (seller_id, buyer_user_id) for DMs; errors on terminal states.

pub fn cancel_booking(&self, user_id: i64, booking_id: &str) -> Result<i64, String>
// Buyer self-cancel: WHERE user_id=? AND status IN ('held','confirmed') —
// ownership enforced in the Db layer, not the handler. Frees the slot if
// held; returns seller_id (the caller DMs the seller).

pub fn list_bookings_for_user(&self, user_id: i64)
    -> Vec<(String, i64, String, String, Option<String>, Option<String>)>
// (booking_id, seller_id, service_name, status, starts_at, expires_at) —
// JOINs services + slots; powers /mybookings.

pub fn expire_held_bookings(&self) -> Vec<(String, i64, i64, String)>
// CHANGED (was -> usize, db.rs:5120): rows transitioned held→expired this
// pass as (booking_id, seller_id, user_id, service_name); the status
// transition is the exactly-once guard for the DM (D5).
```

**Bot commands (register via `handlers::registry::CommandSpec`, `min_role`):**

| Command | Tier | Args | Behavior |
|---------|------|------|----------|
| `/book` | User (unchanged) | `<service_name>` | Unchanged surface, new Db contract: claim-race losers get "Slot no longer available". The reply must say **requested**, not "confirmed" (today's text at `commands.rs:1730` lies) |
| `/slotadd` | Seller (unchanged) | `service \| datetime [\| capacity]` | Capacity clamped to 1 (D1); rejects past timestamps |
| `/mybookings` | **new, User** | — | `list_bookings_for_user(user_id)`; `seller_display` for the seller column; empty-state text |
| `/cancelbooking` | **new, User** | `<booking_id>` | `cancel_booking`; DMs the seller; "not found or already closed" on error |
| `/bookingconfirm` | **new, Seller** (Admin/Owner: any seller's rows) | `<booking_id>` | `confirm_booking(None if admin else Some(user_id))`; DMs the buyer |
| `/bookingdecline` | **new, Seller** (Admin/Owner: any) | `<booking_id>` | `decline_booking`; DMs the buyer |
| `/bookings` | Seller+ (unchanged) | — | Seller/admin view as today; buyers are pointed at `/mybookings` |

**Console routes (method + path + auth + role):**

```
GET  /bookings            session   seller → scope = session.seller_id; admin/owner →
                                    unscoped (orders_page pattern, web.rs)
POST /bookings/confirm    session   form: booking_id; seller own / admin any; log_action audit
POST /bookings/decline    session   form: booking_id; seller own / admin any; log_action audit
POST /bookings/cancel     session   form: booking_id; seller own / admin any; log_action audit
```

Page surfaces (D8): bookings table (buyer, service, slot time, status,
expires, per-status action buttons) + read-only services panel (name, price,
duration, active slot counts) pointing at `/serviceadd` `/slotadd`.
POST-redirect-flash. WCAG AA floor applies (labels, focus, contrast, SR
semantics — design specifies).

### Data model deltas

**None.** `services`, `slots`, `bookings` as-is (`schema.sql:360-402`);
`slots.booked` goes dead (D1), `slots.ends_at` starts being written (D4). The
v1.5 priced-bookings shape (Option A) would add one additive column —
`invoices.booking_id` — and nothing else.

### Failure and edge behaviors

- **Claim race**: decided inside `create_booking`'s transaction; the handler
  never pre-checks (`commands.rs:1715` deleted). Past-slot bookings are
  rejected at claim time.
- **Late confirm** (hold expired, slot still `available`): allowed (D3
  resurrection). Slot taken: `Err("slot already taken")`.
- **Sweeper DM failure**: logged at warn, never propagated — the
  `main.rs:702-707` pattern verbatim.
- **Buyer cancels a confirmed booking**: slot freed, seller DM'd. Sellers are
  never DM'd on expiry (noise control); buyers always are.
- **Multi-tenancy**: every read/write is scoped by `seller_scope` or `user_id`
  inside the `Db` method; handlers only choose the scope from the role.

### Test strategy (verified invariants)

1. **Atomic claim**: 8 `create_booking` calls on one slot — exactly 1 `Ok`,
   7 `Err`, slot ends `held`, one booking row.
2. **State machine (proptest, model-based like the invoice claim test,
   AGENT.md §6)**: no transition out of terminal states; `expired → confirmed`
   only while the slot is `available`; every cancel/decline/expire frees the slot.
3. **Exactly-once expiry DM**: `expire_held_bookings` run twice returns the
   row once.
4. **Tenancy** (two sellers, proptest): seller-scoped confirm/decline/cancel
   and buyer cancel never touch another party's booking.
5. **Time validation**: `create_slot` rejects unparseable/past timestamps;
   `ends_at = starts_at + duration_minutes` (checked math, the MAX_PLAN_DAYS
   clamp pattern).

**Non-goals (Area A, v1.4):** priced bookings (v1.5, pending Q1); class
capacity; service/slot deletion; console services CRUD; buyer console; UTC-only times.

## 3. Area B — MCP per-seller tokens

The table (`schema.sql:331-343`), hash lookup (`mcp/mod.rs:59-87`) and
`get_mcp_token_by_hash` (`db.rs:4104-4120`) all exist; only `authenticate()`
(`mcp/mod.rs:149-177`) doesn't use them. The tools layer was built for this:
handlers already scope every query by `principal.seller_id` (`tools.rs:43-58`,
`447-496`).

### Wiring (D6)

`authenticate()` keeps its signature; the check order becomes:

1. Disabled check unchanged (`mcp/mod.rs:153`) — fail-closed 403 before any
   token inspection.
2. Owner token: `ct_eq(presented, MCP_OWNER_TOKEN)` → `Principal::OWNER`
   (unchanged; back-compat, no migration).
3. Otherwise: `lookup_mcp_token(sha256(presented))` — the helper already
   rejects revoked/expired rows (`mcp/mod.rs:71-79`) →
   `Principal { role: Role::Seller, seller_id: Some(t.seller_id) }`, plus
   best-effort `touch_mcp_token(t.id)` and an `mcp_audit` row with the seller id.
4. Otherwise: the **same** 401 response for unknown/revoked/expired/malformed — no oracle.

**Rate limit** (`principal_key`, `mcp/mod.rs:179-184`):
`principal.seller_id.unwrap_or(principal.role as i64)` — one bucket per seller
at the existing `RATE_RPM = 60` (`mcp/mod.rs:103`); the owner keeps the
role-keyed bucket. Verified invariant: seller ids are real Telegram user ids, never
colliding with the small-integer role keys. **Audit**: `mcp_audit` gains
`seller_id` (below) so "which token did what" is answerable — security will ask.

### Interface contracts

**Db methods:**

```rust
pub fn create_mcp_token(&self, seller_id: i64, name: Option<&str>,
                        expires_at: Option<&str>) -> Result<(i64, String), String>
// (id, raw — returned exactly once). raw = util::gen_token(48); stores the
// SHA-256 hash + 8-char prefix (create_api_key_full convention, db.rs:4061).
// Err at 10 active tokens per seller — the cap lives in the Db, like MAX_CODES.

pub struct McpTokenRow { pub id: i64, pub name: Option<String>,
    pub token_prefix: Option<String>, pub revoked: bool, pub created_at: String,
    pub expires_at: Option<String>, pub last_used_at: Option<String> }
pub fn list_mcp_tokens(&self, seller_scope: Option<i64>) -> Vec<McpTokenRow>
pub fn revoke_mcp_token(&self, id: i64, seller_scope: Option<i64>) -> bool
pub fn touch_mcp_token(&self, id: i64) // last_used_at, best-effort

pub fn mcp_audit(&self, tool: &str, role: &str, seller_id: Option<i64>,
                 ok: bool, detail: Option<&str>)
// CHANGED signature (+ seller_id). All ~10 call sites in mcp/mod.rs
// (320, 332, 355, 363, 439, 449, 463, 475, 500) pass principal.seller_id —
// mechanical, server after core lands the column.
```

**Bot command (Seller+ self-service):**

| Form | Behavior |
|------|----------|
| `/mcptoken create <name> [days]` | Mints, DMs the raw token **once** with a "store it now" warning; reply shows prefix + expiry |
| `/mcptoken list` | Own rows: `id \| prefix \| name \| created \| expires \| last_used` — prefix-masked (`mcp_ab12…`), **never** the raw token |
| `/mcptoken revoke <id>` | Own rows only (`seller_scope = Some(user_id)`); idempotent success text |

Admins/owners manage only their own tokens in v1.4: no admin-mints-for-seller
flow, no MCP tool for token lifecycle (bootstrap stays in the bot).

### Data model deltas

- `mcp_audit` + `seller_id INTEGER` (nullable; additive `ALTER TABLE` guarded
  by `pragma_table_info` — the `invoices.product_id` precedent, `db.rs:374-385`).
  Owner rows stay NULL.

### Failure and edge behaviors

- `MCP_OWNER_TOKEN` unset: endpoint still refuses everything (unchanged
  secure-default-off); DB tokens do **not** enable the endpoint alone.
- Revoked/expired token: same 401 as unknown; `touch` never runs.
- Cap race: two `create` calls at the cap — check and insert are one `Db`
  transaction; the loser errors.
- A seller token can never read another seller's data by passing explicit
  `seller_id` args: tools take the seller from the principal, not from args
  (`tools.rs:453`, now load-bearing — must be tested).

### Test strategy (verified invariants)

1. Owner path byte-identical to today; disabled path identical for every input
   (403 before parse).
2. Unknown/revoked/expired bearer → indistinguishable 401 (one response literal).
3. Seller-principal scoping: a seller token on `list_plans`/`list_posts`/
   `subs_report` returns only that seller's rows, ignoring spoofed args.
4. Rate-limit isolation: exhausting seller A's bucket does not throttle the
   owner or seller B.
5. Cap: the 11th active token errors; revoking one frees the slot.
6. `mcp_audit` rows carry the seller id for seller-principal calls.

**Non-goals (Area B, v1.4):** token scopes/granularity; admin-managed tokens;
MCP tools for token lifecycle; per-token rate tiers; token rotation notices.

## 4. Area C — One-time setup links

Today `/becomeseller` (`commands.rs:1984-2012`) and `/dashboard`
(`commands.rs:2031-2062`) DM a `gen_token(12)` password in plaintext; v1.4
replaces both with single-use 15-minute hashed-token links (D7).

### Design

- **Storage: reuse `web_pending_mfa`** (`schema.sql:178-185`) with
  `purpose='setup_link'`, `totp_secret NULL` — already the generic
  pending-token shape (hash PK, account, purpose, TTL); no new table.
- **Token**: `gen_token(48)`, SHA-256-hashed — the MFA pending-token recipe
  (`web.rs:727-731`).
- **Account creation unchanged**: `/becomeseller` still provisions the seller
  row and web account, but the password is a `gen_token(12)` placeholder
  **never displayed**; `/dashboard` mints only a link.
- **Single-use is atomic**: `get_pending_mfa` (`db.rs:3267-3291`) checks
  neither expiry nor purpose, and `delete_pending_mfa` succeeds whether or
  not a row existed — hence the claim primitive below; GET validates, POST consumes.

**Db methods:**

```rust
pub fn consume_pending_token(&self, token_hash: &str) -> Option<(i64, String)>
// Atomic single-use claim: DELETE FROM web_pending_mfa WHERE token_hash=?1
// RETURNING account_id, purpose. Some = won the token (exactly one caller can).

pub async fn set_web_password_by_id(&self, account_id: i64, password: &str)
    -> Result<(), String>
// Same hash+salt scheme as update_web_password (async, commands.rs:2023),
// addressed by id — the token yields account_id, not a username.
```

**Routes (method + path + auth + role):**

```
GET  /setup/claim?token=…   public   validates hash + purpose + expiry (no consume); renders the password form
POST /setup/claim           public   shared login throttle (AGENT.md §5 throttle);
                                     consume_pending_token → None ⇒ generic "link
                                     invalid or expired" page (one message for expired,
                                     reused, unknown — no oracle); password floor ≥ 6
                                     (/setwebpassword rule, commands.rs:2020); success ⇒
                                     set password, create session, redirect /dashboard
```

**Who may generate:** only the two bot commands, for the requesting chat's own
account (`seller_id` bound from `msg.from.id`); admin-minted links are a non-goal.

### Failure and edge behaviors

- Expired / reused / unknown token → one generic page; the row is gone after
  the first successful POST, so replay fails closed.
- Second `/dashboard` link: the PK is `token_hash`, so re-minting adds
  another live token, valid until use or TTL — acceptable (requester owns
  the chat).
- The link DM never contains a password or any other secret (security review point);
  the claim POST shares the login throttle (token guessing is login-class).

### Test strategy (verified invariants)

1. Single-use: two POSTs with one token — first succeeds, second fails;
   `consume_pending_token` returns `Some` exactly once under concurrent callers.
2. TTL: a 16-minute-old token fails the GET validation and the POST claim.
3. Purpose separation: an MFA `verify` token does not work on `/setup/claim`
   and vice versa.
4. No-password-in-DM: the `/becomeseller` and `/dashboard` replies contain no
   credential material (tests grep the message constructors).
5. The claimed session lands on `/dashboard` as the right account (binding kept).

**Non-goals (Area C, v1.4):** admin-minted links; email delivery; link
revocation UI; migrating the MFA flow onto `consume_pending_token`.

## 5. Area D — Trust pack (process, kept short)

- **`CHANGELOG.md`** — Keep a Changelog 1.1 format; docs backfills 0.1.0 → v1.3
  from AGENT.md §11's decision log, then appends per release.
- **`docs/RELEASING.md`** — runbook: gates → version bump →
  tag `v*` → release CI (`.github/workflows/release.yml`) → artifact smoke →
  **human approval** → publish + date the changelog.
- **Version policy** — Cargo tracks the milestone: v1.4 ships as `0.4.0`
  (`Cargo.toml:3` is `0.1.0`; 0.x = pre-1.0, minors may break). `1.0.0`
  requires: zero open P0/P1 data/money defects; security sign-off on
  `docs/SECURITY.md`; one real seller in production for 30 days.

## 6. Dispatch plan

Ordered; briefs liftable verbatim. Finance first, parallel with everything (its
ruling shapes v1.5, not v1.4); design parallel with server.

1. **Finance** — "Rule on §2 Q1 (booking refund/credit semantics +
   price snapshot); v1.4 ships free-confirmation regardless."
2. **Core** — "Land all Db contracts in §2/§3/§4 in
   `src/db.rs` + the one `schema.sql` migrate (`mcp_audit.seller_id`): booking
   state machine with atomic slot claim, MCP token CRUD with 10-cap,
   `consume_pending_token`, `set_web_password_by_id`, `expire_held_bookings`
   row return. Unit tests per AGENT.md §6; no handler/web changes."
3. **Server** — "Wire bot surfaces per §2/§3/§4: /mybookings
   /cancelbooking /bookingconfirm /bookingdecline /mcptoken (registry specs),
   fix the /book 'confirmed' lie, delete the handler-side availability
   pre-check, replace DM'd passwords in /becomeseller //dashboard with link
   DMs, sweeper buyer DM, `authenticate()` seller wiring + `mcp_audit` call
   sites. No raw SQL; role checks before action."
4. **Design (parallel with server)** — "UX spec for the /bookings
   page (bookings table + confirm/decline/cancel + read-only services panel)
   and /setup/claim, design-system tokens, WCAG AA floor per element."
5. **Web (after design + core)** — "Implement §2/§4 console
   routes in `src/web.rs` exactly as contracted: /bookings + three booking
   POSTs (log_action audit), /setup/claim GET/POST under the shared login
   throttle, POST-redirect-flash, orders_page scoping."
6. **Verification** — "Run gates and verify the
   invariant lists in §2/§3/§4 (atomic claim, state-machine proptest,
   exactly-once expiry DM, two-seller tenancy, MCP 401 indistinguishability,
   single-use setup link). File defects; fix nothing."
7. **Security** — "Review the auth-adjacent surface before
   merge: MCP seller scoping and 401 oracle, setup-link claim (single-use,
   TTL, throttle, no secret in DMs), booking claim transaction. Floor rules;
   owning teams remediate."
8. **Docs** — "Update docs/COMMANDS.md + README command
   lists; init CHANGELOG.md (backfill to v1.3); write docs/RELEASING.md; note
   the setup-link flow in docs/EASY_SETUP.md."

Sequencing rule: no team starts on a contract core has not landed in
`src/db.rs` — the signatures are the interface between domains; drift from
them is a verification defect.
