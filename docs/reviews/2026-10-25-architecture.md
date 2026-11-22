# Architecture Review & Plan — 2026-10-25

**Method.** Three parallel read-only reviews — **architecture** (review +
plan), **server** (server-layer health), **security** (blocking findings).
Load-bearing citations were verified against the source (all confirmed
verbatim) and integrated into this document. Gate status at time of review:
green (fmt / clippy / 64 tests / audit / deny).

**Verdict (architecture).** The core architecture is sound and the ownership design
is real and verified. The **security floor is not met** and money
correctness has races: under the conflict rules, those
findings gate the roadmap — everything else is queued behind them.

## Verified findings (all citations confirmed)

### Floor — blockers, fix before any feature work

| # | Finding | Evidence | Owner | Fix sketch |
|---|---------|----------|-------|------------|
| B1 | Session/API tokens from an LCG seeded by wall-clock nanos — forgeable offline ⇒ full admin takeover | `src/util.rs:42-55`; used at `db.rs:1257`, `db.rs:1573`, `handlers/commands.rs:973` | core | CSPRNG tokens; rotate all existing sessions/API keys/seller passwords after deploy |
| B2 | `/setup` unauthenticated and replayable (guard reads startup-frozen config until restart) | `src/web.rs:590-612`, `main.rs:31` | server | Persist "configured" in DB; close setup on completion; loopback-only in setup mode |
| B3 | Crypto Pay webhook accepts requests with no signature header (check is conditional) | `src/api.rs:3478-3494` | server | Mandatory header + configured token, constant-time compare, fail-closed |
| B4 | Invoice claim grants on API error or missing token (`if let Ok` fall-through) | `src/api.rs:3389-3409` | server | Fail-closed: grant only on explicit `status=="paid"`; fix `getInvoices` envelope parsing (`result.items`, flagged UNVERIFIED vs live API) |
| B5 | Every Telegram user auto-becomes a seller; `/grant` reaches the global paid channel | `handlers/commands.rs:118-120`, `:755-780`, `:1063-1072` | server | Remove unconditional `upsert_seller`; grant only into seller-owned channels |
| B6 | Payment grant is not atomic: three granters, no transaction, mark-paid-before-grant (crash = paid-never-granted; concurrency = double-grant) | `main.rs:143-193`, `commands.rs:592-644`, `api.rs:3370-3453`, `db.rs:536` | server+core | One transactional `claim_and_grant`; UNIQUE(`subscriptions.invoice_id`) |
| B7 | `duration_days` overflow panics the unsupervised payment verifier (silent stop until restart) | `commands.rs:527`, `db.rs:769`, `main.rs:182` | server | Clamp 1..=3650; `checked_add_signed` everywhere |

### High (v1.1, immediately after the floor)

Owner web password defaults to the public `OWNER_ID` (`main.rs:44-53` — server);
stored XSS via unescaped dashboard interpolation (`web.rs:352-422` — web);
cross-seller channel writes, cross-seller `/edit`-family commands
(`api.rs:607-613`, `:1241-1265`, `:1519-1523`, `commands.rs:309-428` — server);
self-marked "paid" invoices inflate payout revenue (`api.rs:739-758` — server);
global plan namespace leaks pricing and mis-resolves `/buy` (`db.rs:724`,
`schema.sql:61` — server/core); API login unrated + web lockout map unbounded
(`api.rs:84`, `web.rs:247-265` — server); `doctor.sh` sources `.env` as shell and
leaks the token via `ps` (`doctor.sh:46`, `:88` — core); `install.sh` echoes
secrets and can be steered by a hostile `--env-file` (`install.sh:232`,
`:190-192` — core); expiry only runs on manual `/expirecheck` (server, S).

### Hygiene (v1.1 backlog)

`Mutex<Connection>::lock().unwrap()` on poisoned mutex (core); bcrypt on the
async worker (core); no-op `auth_password`/`codes_delete` returning success (server);
`gen_token`-based activation codes → crypto-random (core, rides B1); webhook +
scheduled-post retry caps (server); systemd unit extra hardening (core).

## Architecture notes (architecture, accepted into the record)

- Sound: single SQL surface in `Db` (95 `pub fn`, no raw SQL outside it); role
  gating centralized at dispatch head; distribution story shipped; the structure is
  verified.
- Boundary debt: subscription-grant coordination duplicated at 6 call sites
  (`api.rs:560,895,3425`, `commands.rs:631,692,767`) — extract a core
  `services/` layer (this also fixes B6 structurally). `api.rs` (3,671 lines)
  is an axum interface owned by the server layer while its twin `web.rs` is web — ownership
  decision below. `WebState` holds a `Bot` — web performs Telegram side
  effects directly; must go through services before v2 worker split.
- Doc drift: README architecture section omits `api.rs`/`web.rs`; two
  ARCHITECTURE.md files (root vs `docs/`) — fold in v1.1 (docs).

## Plan

### v1.1 — security & money correctness (entry: gates green; exit: floor met)

1. **WS1 Security floor (core→server→web, parallel):** B1 CSPRNG + rotation (core);
   B2 setup nonce/close-on-configure (server); B3+B4 webhook & claim fail-closed +
   envelope fix (server); B5 seller/grant scoping (server); XSS escaping (web);
   API rate limit + bounded lockout (server); doctor/install secret hygiene (core).
2. **WS2 Money correctness:** B6 `claim_and_grant` transaction + UNIQUE index
   + route all three granters through it (server/core, M); B7 clamps (S); expiry
   sweeper on a timer (S); seller-scope middleware for web/API/bot channel
   args (M); plan namespace `(seller, name)` (M).
3. **WS3 Product hardening:** `make backup` via SQLite `.backup` (core);
   backup/restore round-trip test + three-profile smoke in CI (verification); docs
   dedupe (docs); extract inline CSS to the token contract (design).

### v2 — scaling (unchanged; entry: >1k sellers or DB p99 >50ms)

Postgres behind the same `Db` trait; bot/web worker split enabled by the
services seam; `spawn_blocking`/pooling decision revisited with measurements.

## Decisions pending owner approval (proposals, not yet accepted)

1. **api.rs ownership** → *recommend:* move to the web layer after services extraction.
2. **Services layer timing** → *recommend:* v1.1 (fixes B6 structurally).
3. **Backup as product feature** → *recommend:* yes, v1.1.
4. **Connection pooling now?** → *recommend:* no; keep `Mutex<Connection>`,
   document; revisit with latency data.
5. **dialogue.db stays separate in v1** → *recommend:* yes; document in the
   data model (currently unstated).

## Sign-off matrix

| Team | Role in this review | Status |
|------|--------------------|--------|
| Architecture | Review + plan + decisions | ✔ delivered (this document) |
| Security | Blocking findings B1–B5 + HIGH set | ✔ delivered; floor gates WS1 |
| Server | P0/P1 health + feasibility sizing | ✔ delivered; owns most WS1/WS2 |
| Verification | Citation verification + gate snapshot | ✔ verified (5/5 spot-checks exact) |

B1–B7 remediation is implementation work owned
by the named teams; anything touching real payment value during fixes requires owner approval.

---

## Remediation record (2026-10-25, same day)

**Status: WS1 + WS2 + WS3(core) IMPLEMENTED and gated.** Dispatched as three
parallel team-owned changes (core: util/db/scripts; server: main/commands/api;
web: dashboard and pages), integrated centrally, verified by the test suite
(fmt / clippy / **68 tests** / audit / deny) plus live
smoke tests.

| Finding | Status | Where |
|---------|--------|-------|
| B1 forgeable tokens | ✅ fixed | `util.rs` CSPRNG (`getrandom`); one-time migration clears all sessions + API keys; `generate_code` now CSPRNG |
| B2 setup replay | ✅ fixed + **smoke-proven** | DB `setup_done` gates `/setup`; after POST, `/setup` → 303 without restart |
| B3 unsigned webhook | ✅ fixed | signature header mandatory, constant-time compare, 503 when unconfigured |
| B4 fail-open claim | ✅ fixed | claim → verify (robust `items`/`result.items`/array parsing) → grant-if-new → finalize; fail-closed on every error path; user binding (None ⇒ admin/owner only) |
| B5 auto-seller + `/grant` reach | ✅ fixed | unconditional `upsert_seller` removed; sellers resolve only their own channel; global fallback admin/owner only |
| B6 double/missed grant | ✅ fixed | `claim_invoice_for_grant` conditional UPDATE serializes granters; 'granting' re-queues crash recovery; unique index attempted (warn+skip on legacy dupes) |
| B7 overflow panic | ✅ fixed | `MAX_PLAN_DAYS=3650` clamp; `try_days` + `checked_add_signed` (chrono's `Duration::days` itself panics — caught in review of the fix) |
| HIGH owner password default | ✅ fixed | unset `WEB_PASSWORD` → CSPRNG password printed once, never the owner id |
| HIGH stored XSS | ✅ fixed | `esc()` on 13 dynamic interpolation sites |
| HIGH cross-seller writes | ✅ fixed (web/api/bot) | `channel_of(seller)` scoping across ~17 call sites; `/redeem` resolves the code's own seller |
| HIGH self-marked paid | ✅ fixed | manual `paid` transitions rejected (400) |
| HIGH login rate limit | ✅ fixed | shared `web::login_throttle`/`login_ok` used by BOTH web and API logins; bounded map |
| MED doctor/install hygiene | ✅ fixed | no shell-sourcing of `.env`; token never in argv (`curl -K`); installer never prints env values; proxy ⇒ `WEB_BIND=127.0.0.1` unconditional |
| WS3 backup | ✅ shipped | `scripts/backup.sh` (WAL-safe `sqlite3 .backup`), `make backup`; tested |
| Expiry sweeper | ✅ shipped | 6h background job shared with `/expirecheck` |

**Known deferred** (recorded, not lost): seller-scope middleware for the
remaining `/edit`-family bot commands relies on Telegram-side rights (B5
partial); plan namespace `(seller, name)` global-lookup fix (P1) ✅ retired
in v1.2 (`get_plan_global` callers removed); `Mutex<Connection>` pooling
decision (v2, decision 4); api.rs ownership move to web (decision 1, after
services extraction); docs dedupe (docs).
