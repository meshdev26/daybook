# Local hardening evidence — 2026-11-28

This is a factual record of local, reversible (T0) changes made after the T1
plans in `docs/plans/2026-11-28-t1-*.md`. Those plan files are gitignored, so the
decisions and their owner-decision records are mirrored here.

Baseline on HEAD `3955ec4` was all nine gates PASS (self-validation, not an
audit). No T2 gate was executed: no merge, push, tag, release, version bump,
production use, real customer data, real money, live Telegram call, or secret
access. This record is **not** owner security sign-off and **not** a
production-readiness claim.

## 1. Invoice `refunded → paid` race

- Plan: `docs/plans/2026-11-28-t1-invoice-state-race.md`.
- Red: with the `status NOT IN ('refunded','failed')` predicate temporarily
  removed, `cargo test --lib db::tests::test_finalize_invoice_paid_never_overwrites_refunded -- --exact`
  failed: `left: "paid"`, `right: "refunded"`, exit 101.
- Fix: `Db::finalize_invoice_paid` now uses the CAS predicate and logs a skipped
  finalize on `Ok(0)`. Test and fix in `src/db.rs`, `src/db/tests.rs`.
- Green: same command, `1 passed; 0 failed`, exit 0.
- No schema, payment, refund, roles, tenant scope, or analytics change.

## 2. Seller grant channel fallback

- Plan: `docs/plans/2026-11-28-t1-grant-channel-fallback.md`.
- Red (staged tree before the chain replacement):
  - `cargo test --lib services::grant::tests::seller_no_channel_never_grants_into_global_channel -- --exact`
    failed: `left: "paid"`, `right: "paid_ungranted"`, exit 101.
  - `cargo test --bin daybook tests::retry_ungranted_grant_seller_no_channel_never_uses_global -- --exact`
    failed: the retry returned `true`, exit 101.
  - Admin control `admin_seller_keeps_global_channel_fallback` passed before the
    fix and after it.
- Fix: both resolution chains now derive `is_admin` from `roles::role_of` /
  `TenantScope::PLATFORM_ID` and delegate to the existing seller-scoped
  `channel_of`; the REST claim site was consolidated onto the same helper.
  `channel_of` was made `pub` (from `pub(crate)`) because `src/main.rs` is a
  separate binary crate. No role semantics or tenant scope changed.
- Green: all 11 `services::grant::tests::` passed, and the retry test passed.

## 3. Channel authorization bypass

- Plan: `docs/plans/2026-11-28-t1-channel-scope.md`.
- Red: `cargo test --lib handlers::channel_access::tests::channel_gate_refuses_user_binding_with_seller_id -- --exact`
  failed at `assert!(!may_mutate_channel(...))` for `user` + `Some(42)`, exit 101.
- Fix: `may_mutate_channel`, `may_post_to_channel`, and the console
  `may_mutate_post` resolve `Role::tenant_scope` from the session role and
  binding; `Denied` is refused before any ownership comparison. The shared
  gate functions were changed in place instead of changing every wrapper
  signature: the wrappers already pass role and binding as inputs to the
  resolver, so this keeps one policy source with no signature churn.
- Green: all 4 `handlers::channel_access::tests::` passed, including the
  pre-existing console-policy match test.

## 4. Production error paths

- Plan: `docs/plans/2026-11-28-t1-error-paths.md`.
- Red: `cargo test --lib poisoned_lock` produced three panics calling
  `Result::unwrap()` on `PoisonError` at `src/auth.rs:97`, `src/auth.rs:121`,
  and `src/ai/rate_limit.rs:51`, exit 101.
- Fix: poisoned login-throttle lock denies the attempt; a poisoned `login_ok`
  lock skips bucket removal; a poisoned rate-limit lock returns a refusal with
  a 60-second retry. The buyer-only `list_tickets` branch now handles a failed
  `prepare` like its sibling branches instead of unwrapping.
- Green: `cargo test --lib poisoned_lock`, `3 passed; 0 failed`, exit 0.
- `unwrap_or(0)` residuals at `src/api.rs` and `src/web/referrals.rs` were
  removed in favour of the already-checked `seller_namespace()`. Source-level
  red was `grep -n 'seller_id.unwrap_or(0)'`; after the edit grep found no
  matches.
- `docs/SECURITY.md` pointer for the `invoices_claim` carve-out was corrected to
  the verified current lines. This is a pointer correction only, with no gate
  claimed.

## 5. Deferrals and reasons

- The backup/restore round-trip test in the plan was **not added**. Both scripts
  parse `$ROOT/.env` and export its variables before resolving `DATABASE_DIR`;
  `.env` is present in this tree. A test invoking the scripts could therefore
  be redirected at the real data directory. Adding it would require touching
  secret handling, which is out of scope here.
- The `Db` write-helper signature changes in the plan were **not applied**.
  They ripple through bot and console callers and include money-adjacent
  helpers; that is a broader decision than the enumerated error-path items.
- The `list_tickets` `prepare` fix has source-level verification only. An
  attempted behavioral red failed for the wrong reason (migration rejected the
  malformed fixture schema), so no behavioral red is claimed for that single
  site.

## 6. Verification status

The red→green evidence above is local self-validation. On the final formatted
tree, `cargo fmt --check`, `cargo clippy --lib --all-targets -- -D warnings`,
`cargo check --locked`, `cargo audit` (only the known allowed `rustix` warning)
and `cargo deny check licenses bans sources` were observed passing. In an
earlier gate run, `cargo test --lib` (817 passed), `cargo test --test
tenant_isolation` (13 passed) and `cargo test --test adversarial_local`
(13 passed) were observed passing on the tree that differs from the final tree
only by `cargo fmt` whitespace changes.

The `--features moderation` sweep and a post-format rerun of the test targets
could not be completed locally: repeated harness server restarts cancelled the
runs three times. No red result was produced by those cancellations; they are
not failed gates. CI status must be read live from GitHub Actions after any
push, not from this record. Until a complete local run is observed, individual
items are `implemented-but-verified-locally-red-green` and the combined tree is
not claimed as fully verified. External gates remain open:
independent pentest, scratch-deployment verification, owner security sign-off,
one real seller in production for 30 days, and owner-executed tag/release.
