# Money Safety Spec — Approved Safety Batch (T1)

**Status:** T1 proposal. Owner-approved scope is L11 recommendation B: **implement
Decisions 1–5; defer Decisions 6–12 behind the explicit guards in §6.**
This document specifies money *semantics* only. It contains no implementation
claim: every "MUST" below is a requirement for L8/L9/L7, not a statement that
the code already satisfies it.

**Branch:** `feature/multibot-foundation`. **Owning rung:** L5 (Senior Finance)
decides the numbers/state semantics; L9 owns schema, L8 owns logic, L7 owns the
Finance page if the UI needs changes, L10 holds a blocking review, L3 runs the
gate set (§7). Human T2 gates are listed in §7.

**Evidence baseline (exact locations in this tree):**
- `schema.sql:28-42` invoices (`amount`/`asset` TEXT, `status`, `attempts`);
  `schema.sql:222-233` refunds (no idempotency key, no uniqueness, no creator);
  `schema.sql:236-245` payouts; `schema.sql:66-78` subscriptions;
  `schema.sql:96-104` product_grants (`invoice_id TEXT NOT NULL UNIQUE`);
  `schema.sql:121-132` activation_codes (`max_uses … DEFAULT 1` but writable);
  `schema.sql:52-63` plans (`duration_days INTEGER NOT NULL`);
  `schema.sql:305-320` promo_codes (`value REAL`), `schema.sql:359` referral_codes.
- `src/db.rs:4922-4955` `create_refund` — plain INSERT, no cap, no idempotency;
  `src/db.rs:5066-5083` / `src/db.rs:5231-5248` CAS entry to `executing`;
  `src/db.rs:484` WAL + `busy_timeout=5000` (the serialization primitive the cap relies on);
  `src/db.rs:5887` pending-expiry sweep (status change, never row deletion);
  `src/db.rs:2647` `INSERT OR REPLACE INTO plans` (identity-destroying; see G2);
  `src/db.rs:3615` code-use exhaustion check.
- `src/api.rs:3086-3210` refund create (seller clamped at :3136-3181, asset locked
  at :3136-3148, admin/owner left unclamped at :3179/:3183-3187);
  `src/api.rs:3212-3273` refund approve is `has_level(Role::Admin)` at :3223;
  `src/api.rs:3275-3335` refund execute is `has_level(Role::Admin)` at :3295;
  `src/api.rs:2566-2621` payout approve is owner-only at :2577-2579;
  `src/api.rs:2656-2667` payout execute is owner-only;
  `src/api.rs:2634-2654` API failure classifier;
  `src/api.rs:4570` granter consumes `active` rows, never `paid`;
  `src/api.rs:1393` `max_uses` issuance, `src/api.rs:902` plan-day clamp;
  `src/api.rs:949-977` plan update path; `src/api.rs:3509` finance export.
- `src/web/finance.rs:100-109` second, divergent failure classifier;
  `src/web/finance.rs:118` "admin or the owner" banner;
  `src/web/finance.rs:396` two-person payout rule text.
- `src/crypto.rs:112-117` `transfer(user_id, asset, amount)` — body has **no**
  `spend_id`; `docs/CRYPTO_PAY.md:19-21` server-side verification rule;
  `docs/CRYPTO_PAY.md:57-59` the `CAST(amount AS REAL)` cross-asset defect;
  `docs/SECURITY.md:46` owner approval for money movement;
  `docs/SECURITY.md:54` CAS/ambiguous-transfer policy;
  `docs/API.md:180-189`, `docs/COMMANDS.md:98` current admin/owner wording;
  `docs/CRYPTO_PAY.md:45-46` "owner/admin flow" wording (to be corrected by D2).

---

## 1. Decision 1 — Refund uniqueness, cumulative cap, immutability

**Problem:** `refunds` has no uniqueness (`schema.sql:222-233`) and
`create_refund` is an unguarded INSERT (`db.rs:4922-4955`). A blanket
`UNIQUE(refunds.invoice_id)` is **rejected**: it would break partial refunds,
break re-issue after a definite failure, and fail to open any legacy DB that
already holds more than one refund for an invoice.

### 1.1 Exact SQL-level invariants (L9)

```sql
-- (a) At most ONE in-flight refund per invoice. Terminal rows do not block a
--     fresh, distinctly-keyed attempt. 'needs_reconciliation' is in-flight.
CREATE UNIQUE INDEX IF NOT EXISTS uq_refunds_invoice_inflight
    ON refunds(invoice_id)
    WHERE status IN ('pending','approved','executing','needs_reconciliation');

-- (b) An idempotency key is unique per invoice for ALL time, including after
--     execution. A retry with the same key returns the existing row; it can
--     never create a second money movement.
CREATE UNIQUE INDEX IF NOT EXISTS uq_refunds_invoice_idem
    ON refunds(invoice_id, idem_key);

-- (c) A refund's asset is locked to the invoiced asset (cross-table rule).
CREATE TRIGGER IF NOT EXISTS trg_refund_asset_matches_invoice
BEFORE INSERT ON refunds
WHEN NEW.invoice_id IS NOT NULL
 AND NEW.asset IS NOT (SELECT asset FROM invoices WHERE invoice_id = NEW.invoice_id)
BEGIN SELECT RAISE(ABORT, 'refund_asset_mismatch'); END;

-- (d) Money-bearing rows are immutable. Only status, updated_at and the
--     decision-5 resolution columns may ever change.
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
```

New columns on `refunds` (all non-money types): `idem_key TEXT NOT NULL`,
`created_by INTEGER NOT NULL`, `created_role TEXT NOT NULL`,
`executing_since TEXT`, `resolution TEXT`, `resolved_by INTEGER`,
`resolved_at TEXT` (the last four are consumed by Decision 5). No column is
`REAL`. `REFUND_STATES = {pending, approved, executing, needs_reconciliation,
executed, void}`; `processed` is retained as an accepted legacy alias of
`executed` for reads only and is never written by new code.

### 1.2 Creation algorithm — one `BEGIN IMMEDIATE`

`create_refund` MUST execute this whole block in a single `BEGIN IMMEDIATE`
transaction (`db.rs:484` gives WAL + busy timeout; the immediate transaction is
what serializes two concurrent creators so both cannot pass the cap):

1. Load `invoices` row by `invoice_id`; require `status = 'paid'` (same rule as
   `api.rs:3123-3131`). Abort 404/400 otherwise.
2. Parse the stored invoice `amount` and every counted refund `amount` as a
   **base-10 decimal in integer minor units (scale 8, `i128`)**. Never `f64`,
   never `CAST(... AS REAL)` (`CRYPTO_PAY.md:57-59`). Non-finite, negative or
   unparseable legacy amounts ⇒ abort 409 `unquantifiable_legacy_refund` until
   an owner resolves them through the audited Decision-5 resolution path.
3. `SELECT COALESCE(SUM(minor_units),0) FROM refunds WHERE invoice_id=? AND
   status <> 'void'` — counted states are all rows including terminal
   `executed`/`processed`; a reverted attempt is still counted once, which is
   deliberately conservative.
4. Require `cumulative + requested_minor <= invoice_minor`; otherwise abort 409
   `refund_cap_exceeded`. `requested` defaults to the full invoiced amount
   (`api.rs:3150-3152`); sellers are clamped and asset-locked
   (`api.rs:3136-3187`); **this cap and asset lock now apply to admin and owner
   too**, closing the gap at `api.rs:3179`/`:3183-3187`.
5. `INSERT` with `idem_key`, `created_by`, `created_role`, status `pending`.
   A `uq_refunds_invoice_inflight` or `uq_refunds_invoice_idem` violation is
   translated to a **200 with the existing row id** for a repeated idem key
   (no new audit money event), or 409 `refund_in_flight` for a distinct key.

### 1.3 Settlement effect

- **Full refund** (`cumulative >= invoice amount`, exact minor-unit equality):
  set `invoices.status = 'refunded'`; revoke the subscription/product grant and
  kick the buyer (the existing execution effect at `api.rs:3279-3282`).
- **Partial refund** (strictly less): `invoices.status` **stays `paid`**; no
  grant revocation, no kick. Audit the remaining minor units and asset. This is
  the explicit reversal of any reading of `docs/CRYPTO_PAY.md:74-76` ("refunded
  at refund spawn") as a global rule — that sentence is booking-specific; the
  general invoice rule is at execution and is binary as above.

### 1.4 Legacy-data handling

- **Do not add `UNIQUE(refunds.invoice_id)`; do not delete or merge legacy
  refund rows.** Auditability requires every historical money movement stay
  reconstructable (`docs/SECURITY.md:46`, L5 charter), which is exactly the
  pattern already used for appeal dedup at the tail of `schema.sql` — except
  that the dedup cleanup there DELETEs rows, which is **forbidden for money**.
- Migration preflight: if any `invoice_id` has >1 row in the in-flight status
  set, migration **fails closed** with the list of invoice ids. Owner resolves
  each duplicate by an audited Decision-5 `void` resolution (one survivor),
  then re-runs migration. Migration is idempotent once clean.
- Backfill: `idem_key = 'legacy:' || id` (unique by PK); `created_by`/
  `created_role` from the `refund_create` audit record (`api.rs:3201-3208`)
  where one exists, otherwise the sentinels `-1` / `'unknown'` — never guess an
  actor. Backfilled rows keep their original amount/asset/status.

---

## 2. Decision 2 — Owner-only approve **and** execute; creator separation

**Current gap:** refund approve (`api.rs:3223`) and execute (`api.rs:3295`)
accept any `Admin+`; creators are not recorded, so an admin can create and then
approve/execute their own refund. Payouts already require owner for both
(`api.rs:2577-2579`, `api.rs:2656-2667`, `web/finance.rs:396`).

**Required semantics — every money row, both surfaces (REST, bot, web):**

| Action | Roles allowed | Additional guard |
|---|---|---|
| Create refund / payout | Seller, Admin, Owner (creator recorded) | Invoice/asset/cap rules of §1; `created_by`/`created_role` persisted |
| Approve | **Owner only** | `403 owner_required` for Admin/Seller; `403 creator_forbidden` if `actor_id == created_by` |
| Execute | **Owner only** | same creator check; CAS `approved → executing` still required (`db.rs:5066`, `db.rs:5231`) |
| Void / resolve (D5) | **Owner only** | non-empty audited reason |

- The approver MAY also be the executor — this mirrors the unchanged payout
  flow. The two distinct persons are the **creator** and the **approver/
  executor**. A refund created by the sole owner therefore requires a second
  owner account to approve; there is no break-glass self-approval. Owners
  needing a refund on a single-owner install MUST use the system-created path
  in §3 (`created_by = -1`, no human creator) instead of creating it by hand.
- System-created refunds (`created_role = 'system'`) carry no human creator and
  are exempt from the creator check only; they still require an Owner to
  approve and execute.
- `/refundapprove` (`docs/COMMANDS.md:98`, `src/handlers/commands.rs:173`,
  owner-only bot gate note at `commands.rs:4829`) and the web Finance actions
  MUST enforce the identical rule — no path may be admin-approvable.
- **Reconciliation with published guarantees:** `docs/SECURITY.md:46` says
  refunds/payouts require owner approval before any transfer; tightening
  refunds from `Admin+` to Owner makes the implementation match that text, and
  `docs/SECURITY.md:54`'s CAS/ambiguous policy is preserved unchanged. Correct
  the now-stale wording in `docs/API.md:181-183`, `docs/COMMANDS.md:98`,
  `src/web/finance.rs:118` ("admin or the owner" → "the owner"), and
  `docs/CRYPTO_PAY.md:45-46` ("owner/admin flow" → "owner-only flow").
- Every approve, execute, void and resolution writes an `audit_log` row with
  `actor_id`, `actor_role`, `action`, `target_id`, and metadata carrying
  `invoice_id`, `amount`, `asset`, `idem_key`, `spend_id` (where applicable)
  and the human reason (`api.rs:3201-3208` is the pattern).

---

## 3. Decision 3 — Paid-but-ungranted recovery

**Current gap:** the granter consumes only `active` invoices (`api.rs:4570`), so
an invoice verified `paid` whose grant claim missed is stranded; verification
may also lag (`CRYPTO_PAY.md:21`).

1. **Never terminal `failed` once paid.** `UPDATE invoices SET status='failed'
   WHERE status IN ('paid','paid_ungranted')` MUST be refused (DB trigger or
   CAS guard) — audit `invoice_failed_after_paid_blocked`. `failed` remains
   legal only from pre-payment states on definitive non-payment proof.
2. **New state `paid_ungranted`.** When server-side verification returns `paid`
   (`CRYPTO_PAY.md:20`) but the exactly-once grant claim produced no
   subscription/grant, set `status='paid_ungranted'` and stamp
   `paid_ungranted_at`. Audit `invoice_paid_ungranted` with invoice id, seller,
   amount, asset. The row stays non-terminal and visible to the recovery job.
3. **30-second retry.** A background job re-runs the exactly-once grant claim
   for `paid_ungranted` rows every 30 s (fixed interval). Each attempt is
   audited `invoice_grant_retry` with attempt number and outcome. Successful
   claim ⇒ status back to `paid`, job stops for that invoice. The 24 h clock is
   measured from `paid_ungranted_at`, not from each retry.
4. **24 hours ungranted ⇒ auto-refund row + owner alert.** The sweeper creates
   one `pending` refund for the full invoiced amount in the original asset,
   with `idem_key = 'auto:ungranted:<invoice_id>'`, `created_by = -1`,
   `created_role = 'system'`, subject to all §1 invariants (so a manual refund
   already in flight suppresses it via the partial unique index). It audits
   `invoice_ungranted_refund_queued`, DMs the owner ("refund queued — approve
   in Finance") and the buyer ("refund queued"), and shows a Finance banner.
   Approval/execution remain the owner-only flow of §2. Buyer sees "refund
   sent" only after execution.
5. **72 hours ungranted ⇒ blocking escalation.** At 72 h from
   `paid_ungranted_at`, if the invoice is still `paid_ungranted` or its
   auto-refund is not `executed`, emit audit `invoice_ungranted_escalation`
   marked BLOCKING, DM the owner, and refuse **new payout creation for that
   seller** with `409 unresolved_ungranted_liability` until the invoice is
   `refunded` or a grant exists. The blocking banner cannot be dismissed by a
   seller; only resolution clears it. These 24 h/72 h values are internal
   operational triggers, **not** a customer SLA (see G7).
6. **Manual `failed` requires Owner + reason.** Any operator-forced transition
   to `failed` (from a non-paid state only) requires `Role::Owner` and a
   non-empty reason persisted as audit `invoice_manual_failed`; empty reason ⇒
   400 `reason_required`, non-owner ⇒ 403. `created_at`-based sweeps never set
   `failed` on a paid row.

---

## 4. Decision 4 — `spend_id` on every transfer + one shared fail-closed classifier

**Current gap:** `CryptoPay::transfer` posts only `{user_id, asset, amount}`
(`crypto.rs:114-117`) — no idempotency key. Three divergent textual
classifiers exist: `api.rs:2634-2654`, `web/finance.rs:100-109`, and the web
action path (`web/finance_actions.rs:499-511`, `:623-704`). Both published
classifiers default **unknown error text to DEFINITE**
(`web/finance.rs:105-109` returns "definitely rejected" for anything without a
known transport prefix; `api.rs:2634-2654` returns "not ambiguous" for the
same), so a novel error can revert an executed transfer and cause a double
payout. This is fail-open and MUST be replaced.

### 4.1 Spend-id discipline

- Add `spend_id: &str` to `CryptoPay::transfer` and include
  `"spend_id": <uuid-v4>` in the request body. One spend_id per transfer
  attempt, globally unique.
- New append-only table `transfer_attempts(id, row_type, row_id, attempt_no,
  spend_id TEXT NOT NULL UNIQUE, created_at, outcome, resolved_at)`.
- **Persist before call:** in a single committed transaction, insert the
  attempt row with a fresh spend_id; only then call `transfer`. No transfer
  call may exist outside the one helper that does insert-then-call. A crash
  after insert leaves a recorded spend_id for reconciliation; a crash before
  insert means no money left the app.
- **Ambiguous retry reuses:** when reconciling an `Ambiguous` attempt, retry
  with the **same persisted spend_id** (Crypto Pay idempotency returns the
  prior result), never a new one — a new key on an unknown-outcome transfer is
  a double-payout risk.
- **Definite retry is fresh:** only after `DefiniteRejected` (API received and
  refused the request) may the caller mint a new spend_id and a new attempt
  row. `attempt_no` increments monotonically per `(row_type, row_id)`.
- A duplicate spend_id insert is a hard error, never an overwrite.

### 4.2 One shared classifier (web + API)

- Define in one shared module a single function returning an enum:
  `TransferOutcome::DefiniteRejected | TransferOutcome::Ambiguous`.
  Define `DefiniteRejected` **only** for positively recognized API refusals: a
  parsed `ok:false` business rejection (e.g. `TRANSFER_FAILED`, insufficient
  funds) or HTTP 4xx other than 408. **Everything else ⇒ `Ambiguous`**:
  5xx, 408, timeout, connect/reset/broken pipe, decode/build errors, empty
  string, and any unrecognized or future error shape.
- Delete `transfer_outcome_ambiguous` (`api.rs:2634`) and
  `transfer_rejected_definitely` (`web/finance.rs:100`); REST, bot and web all
  call the shared enum. Callers branch on the enum, never on a booleans-with-
  inverted-default heuristic.
- `DefiniteRejected` ⇒ revert claim `executing → approved` for retry (502
  `transfer_failed`, `db.rs:5088-5109`). `Ambiguous` ⇒ leave the row
  `executing` and hand it to Decision 5 (503 `execution_ambiguous`), audited
  as `payout/refund_execute_ambiguous`. Never auto-settle an Ambiguous result.
- Tests: a table of recognized definite strings; every other string including
  random fuzz input ⇒ `Ambiguous`; both callers asserted to use the shared
  function.

---

## 5. Decision 5 — Executing-row state machine with reconciliation deadlines

Applies to **both** refunds and payouts. The existing CAS entry to `executing`
(`db.rs:5066-5083`, `db.rs:5231-5248`) is unchanged; this adds a clock and an
audited exit path. On CAS entry, stamp `executing_since` (UTC).

1. `executing` **> 30 min** ⇒ sweeper moves the row to
   `needs_reconciliation` (audit `transfer_reconciliation_due`). No transfer is
   attempted and no state is guessed. The spend_id and attempt rows remain
   attached.
2. `needs_reconciliation` **> 24 h** from entering that state ⇒ audit
   `transfer_reconciliation_overdue` and raise a blocking owner alert (same
   Finance-page severity as §3.5). The row is **never** auto-marked executed or
   failed; neither timeout is evidence of money movement.
3. **Owner decision required** to resolve. Exactly three outcomes, each
   requiring an Owner actor and a non-empty `resolution` reason:
   - `executed` — operator proves the transfer landed (receipt checked); apply
     the refund/payout success effects of §1.3 / the existing execution path.
   - `reverted_to_approved` — operator proves the transfer never landed; row
     returns to `approved` with a **fresh** spend_id on the next attempt (§4.1).
   - `void` — cancel with reason; refunds never re-executable, invoice stays
     `paid`, cumulative cap still counts the row as spent for audit honesty.
   Write `resolution`, `resolved_by`, `resolved_at`; audit
   `transfer_reconciliation_resolved` with spend_id, outcome, actor, reason.
   Resolution is append-only — a second resolution attempt is rejected 409.
4. **Daily digest:** once per 09:00 UTC, emit one owner DM/console digest and
   one audit `transfer_reconciliation_digest` listing every currently
   `executing` or `needs_reconciliation` row with id, seller, amount+asset,
   age, spend_id, plus any past the 24 h deadline. Include a hash of the listed
   row ids in the audit metadata. An empty digest still writes an audit row
   (proof the job ran); a failed/locked digest run alerts the owner rather than
   being silent.
5. Timers MUST be driven by an injectable clock so L3 can test 30 min / 24 h /
   72 h boundaries without sleeping. These are internal operational targets,
   not SLA commitments (G7).

---

## 6. Deferred guards (Decisions 6–12) — explicit, enforceable constraints

Decisions 6–12 are **not implemented in this batch**. Each is replaced by a
guard that MUST hold on every release until the decision is lifted by L5.
Verification is per-release and is a release gate, not a one-time check.

| ID | Guard (enforceable constraint) | Per-release verification |
|---|---|---|
| G1 | `activation_codes.max_uses` MUST be 1 for every value-bearing code (any row with `plan_id`/`value_days`). Reject issuance with `max_uses > 1` at `api.rs:1393` and the bot `/gencodes` path (400 `multi_use_codes_disabled`). Existing multi-use rows are read-only and non-renewable; redemption may drain them (`db.rs:3615`) but no new row may be issued. | Query: `SELECT count(*) FROM activation_codes WHERE max_uses > 1 AND created_at > <cutover>` must be 0; unit test rejects `max_uses=2`; grep shows generation endpoints clamp to 1. |
| G2 | `plans.duration_days` (`schema.sql:58`) is immutable once any `invoices.plan_id` references that plan: update path `api.rs:949-977` / `db.rs:2647` returns 409 `plan_invoiced_immutable`. New plans set duration once, clamped to `1..MAX_PLAN_DAYS` (`api.rs:902`). `INSERT OR REPLACE` at `db.rs:2647` MUST NOT be used for an existing plan (it destroys row identity); use `UPDATE`. | Test: create plan → invoice → PATCH duration ⇒ 409; plan id unchanged after edit. Code review of `db.rs` plan-write paths; grep: no `INSERT OR REPLACE INTO plans` on a write path for existing rows. |
| G3 | No NEW `REAL`/`DOUBLE`/`FLOAT` money column. All amount/price/value columns stay TEXT (`schema.sql:32,56,85,227,239`). `CAST(amount AS REAL)` cross-asset summing stays prohibited (`CRYPTO_PAY.md:57-59`). Existing `promo_codes.value REAL` (`schema.sql:309`) may not be used for money (see G5). | Migration diff review for new REAL columns; `grep -iE 'REAL|DOUBLE|FLOAT' schema.sql` reviewed and limited to the pre-existing promo column; test asserts money columns are TEXT. |
| G4 | AI charging is OFF: `src/ai/**`, `src/web/ai.rs`, `src/web/ai_actions.rs` perform no invoice/debit/credit/balance write and expose no config flag to enable charging. Cost accounting, if present, is observational only. | Grep money-write symbols in AI modules = none; test: one AI request creates zero invoice rows and zero money `audit_log` events. |
| G5 | Promo / referral / bonus / credit are INACTIVE: `promo_codes` (`schema.sql:305-320`), `referral_codes` (`schema.sql:359`) and any bonus/credit balance cannot change an invoice amount or grant value; no new rows issued by this batch. | Grep `promo|referral|bonus|credit` in checkout/buy paths (`src/web/cart.rs`, `api.rs`) shows no amount mutation; test: applying a promo leaves invoice amount unchanged. |
| G6 | Fee = 0 and no entitlement formula: no platform fee/commission/spread is deducted and no pro-rata or remaining-days formula is used. Refund = `min(requested, invoiced amount)` per §1.2 only (a 29-of-30-days plan refunds the full invoiced amount when requested full). No fee column. | Test: full refund of a mostly-consumed plan equals invoiced amount; grep `fee|commission|prorat|entitlement` in money code = none. |
| G7 | No retention, export or SLA commitment is made: no retention period, export SLA or uptime/response SLA for money data in docs or code. `finance_export` (`api.rs:3509`) is a convenience, not a contract. All timers in §3/§5 are labelled "internal operational target, not an SLA" in the docs. | Release-note and doc review for retention/export/SLA promises; this spec's timer sections carry the explicit label. |
| G8 | No data-deletion regime: no automatic or self-service DELETE/anonymize of `invoices`, `refunds`, `payouts`, `audit_log`. Refund rows are append-only (§1.1). Sweepers change status only (`db.rs:5887` pattern). | Grep `DELETE FROM (invoices\|refunds\|payouts\|audit_log)` = none; test: sweeper run leaves money-table row counts unchanged. |

---

## 7. Ownership and gates

| Rung / gate | Responsibility in this batch | Deliverable |
|---|---|---|
| L11 Fellow Architect | Owns the rec-B scope boundary and defers 6–12; arbitrates any proposed guard relaxation. | Written scope decision; no code. |
| L9 Staff Core Systems | Schema + `Db::migrate` only: §1.1 indexes/triggers/columns, §3 `paid_ungranted` + guard, §4 `transfer_attempts`, §5 clock columns. No logic. | Migration + schema diff; legacy-DB migration test. |
| L8 Staff Server | All logic: §1.2 cap transaction, §2 gating + audits, §3 sweepers/alerts, §4 spend-id helper + shared classifier, §5 state machine + digest; REST/bot/web parity. | Code + unit/integration/property tests. |
| L7 Staff Client Platforms | **If needed**: Finance page owner-only actions, creator hiding, `needs_reconciliation` resolution form with reason, banners, digest rendering. No new money semantics in the UI. | Web changes only where the UI cannot express §2/§5. |
| L10 Principal Security | **Blocking review** of fail-closed classification, separation of duties, spend-id idempotency, trigger correctness, no money-row deletion, audit completeness. May block release; findings fixed by the owning rung, not by L10. | Sign-off or blocking defect list. |
| L3 Associate Verification | Gate set: `cargo build`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo audit`, `cargo deny check`, `python3 scripts/verify-agent-tiers.py`, plus this spec's property/fuzz/migration/clock tests. Verifies; never fixes. | Green gate report. |
| Human T2 — first real-value transfer | Owner present; first live refund/payout executed for a **small amount** and the Crypto Pay receipt checked before any larger transfer is enabled. | Signed owner confirmation + receipt reference. |
| Human T2 — keys | Crypto Pay token and owner credentials rotated/scoped and confirmed not logged before transfers are enabled. | Owner confirmation. |
| Human T2 — publishing | Owner approves the corrected docs (`SECURITY.md`, `API.md`, `COMMANDS.md`, `CRYPTO_PAY.md`) and release notes. | Owner approval. |
| Human T2 — deletion | **None in scope** (G8 forbids money-row deletion). | No gate. |

---

## 8. Acceptance criteria

**Implementation (L9/L8/L7, as assigned above)**
- [ ] I1 — §1.1: both partial unique indexes, the asset-match and immutability
      triggers, and all new columns exist; no blanket `UNIQUE(invoice_id)`.
- [ ] I2 — §1.2: refund creation runs inside one `BEGIN IMMEDIATE` with decimal
      minor-unit cap, asset lock and idem handling for **all** roles.
- [ ] I3 — §1.3: full refund flips invoice to `refunded` and revokes the grant;
      partial leaves it `paid` with no kick.
- [ ] I4 — §1.4: migration backfills ids/actors, fails closed on in-flight
      legacy duplicates, deletes no money rows, and is idempotent.
- [ ] I5 — §2: approve/execute are Owner-only with creator separation on REST,
      bot and web; all four actions audited; stale docs wording corrected.
- [ ] I6 — §3: `failed` blocked after paid, `paid_ungranted` + 30 s retry +
      24 h auto-refund/alert + 72 h blocking payout hold + owner+reason manual
      failure, all audited.
- [ ] I7 — §4: every transfer has a pre-persisted spend_id; ambiguous retries
      reuse it; definite retries are fresh; one shared enum classifier wired
      into web and API; old classifiers removed.
- [ ] I8 — §5: 30 min → `needs_reconciliation`, 24 h owner decision, three
      audited resolutions, 09:00 UTC daily digest, injectable clock.
- [ ] I9 — §6: all guards G1–G8 hold; no deferred feature is wired into money.

**Verification (L3 + L10 + humans)**
- [ ] V1 — Property tests: cap monotonicity (sum never exceeds invoice);
      classifier fuzz (any unrecognized string ⇒ `Ambiguous`); idem-key replay
      creates exactly one row; no `paid → failed` path.
- [ ] V2 — Migration test on a legacy fixture containing duplicate in-flight
      refunds: fails closed, then opens after audited `void` resolution; no row
      deletions; backfilled `idem_key` unique.
- [ ] V3 — Integration tests with injected clock at 30 s / 24 h / 72 h and
      30 min / 24 h boundaries; concurrent creators cannot both pass the cap;
      concurrent executors still lose the CAS (existing tests extended).
- [ ] V4 — Grep/assertion guards G1–G8 executed in CI or the release checklist.
- [ ] V5 — Full L3 gate set green (§7) and L10 blocking review signed.
- [ ] V6 — Human T2 gates: small-amount first transfer + receipt, keys, docs/
      publishing approval; deletion gate confirmed as "none in scope".

**Out of scope / non-goals for this batch:** no new money product, no REAL money
column, no AI charges, no promo/referral/bonus/credit value, no fees, no
entitlement/proration, no SLA/export/retention promise, no data deletion, no
implementation work performed by this T1 document.
