# Verification runbook (local only)

Command checklist for a human verifier. No results are recorded here; no gate is
asserted green and no verification is claimed to have been run.

## 1. Boundary

Loopback and wiremock only. Fresh `tempfile` DB, placeholder tokens; no real money,
no live Telegram, no `data/` or `.env` reads (tests/tenant_isolation.rs:1-12, :58, :124-126).
Expected: unique per-tenant markers and no traffic beyond loopback.

## 2. Environment

Start with an empty `git status`. Toolchain pinned in `rust-toolchain.toml` (1.99.1);
prefer rustup cargo, since a system cargo can ignore the pin (docs/RELEASING.md:23, :32-37).
Record the toolchain version and every command's exit code.

## 3. Gates

`cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`, `cargo audit`,
`cargo deny check`, `cargo build` (or `make test`, Makefile:32-42).
Expected: exit code 0 each. Gate health only, NOT penetration-test evidence.

## 4. Isolation suite

`cargo test --test tenant_isolation`; expect all 13 tests to pass:
`rest_reads_expose_only_the_callers_rows`, `rest_single_item_and_admin_reads_fail_closed_for_sellers`, `rest_writes_ignore_forged_owner_fields_and_preserve_victim_rows`, `mcp_read_tools_are_scoped_per_seller_and_owner_sees_both`, `mcp_write_tools_ignore_bot_and_seller_spoofs`, `mcp_sensitive_and_identity_tools_refuse_both_sellers_before_validation`.
`console_pages_and_posts_are_tenant_scoped`, `bot_resolution_follows_seller_and_channel_ownership`, `expire_check_must_not_touch_another_tenants_subscriptions`, `mutating_routes_require_session_and_never_mutate_another_tenant`, `tenantless_console_session_cannot_write_in_the_global_namespace`, `tenantless_rest_session_cannot_read_or_write_other_tenants`, `tenantless_console_session_cannot_read_other_tenants`.
Expected: own markers present, foreign markers absent, forged ids leave `victim_snapshot` unchanged, B/primary bots get zero requests, tenantless sessions refused.

## 5. Bot regression

`cargo test --lib expirecheck_scopes_seller_and_keeps_owner_global` (src/handlers/commands/tests.rs:751).
Expected: pass; seller A's `/expirecheck` revokes only A's rows, owner keeps the global sweep.

## 6. Scratch deployment boundary

`make doctor` on a scratch instance reaches Telegram `getMe` when `TELEGRAM_BOT_TOKEN` is set (scripts/doctor.sh:98-112).
Expected: token accepted or rejected is reported; operator diagnostics only, never isolation evidence; no real money.

## 7. Sign-off

Date each observation; keep issue #1 open. Publish, tag and push are human-only actions.
Verbatim sentence that must remain (docs/SECURITY.md:46):
"this is not an independent multi-tenant penetration test or a third-party audit, and the scratch-deployment verification in issue #1 remains open."
