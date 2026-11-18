# Multi-Bot Foundation — Implementation Plan (L9 / L8 / L7)

**Specs:** `IA_MULTIBOT_AMENDMENT_SPEC.md` (committed `68a159c`) and the L11 decision memo delivered 2026-11-18.
**Tier:** T1 design approved; implementation is T0 Execute once preconditions clear.
**Ownership:** L9 (schema/config/features), L8 (dispatcher/handlers/moderation hooks), L7 (selector + web surfaces). L10 is a floor on moderation. L3 verifies everything. No rung verifies its own work.
**Scope of this plan:** foundation only — bot-type capability registry + cross-cutting moderation layer. The AI shop bot is the first vertical, planned separately after this lands.

## Preconditions (must hold before any code)

- [ ] L3 re-verification of the IA redesign is **VERIFIED** (currently running).
- [ ] L10's F1/F2 clearance stands; `cargo audit`, `cargo deny check`, `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` green.
- [ ] Owner has ruled on the non-goal guardrails: external AI API calls only (no training/local models); no public non-Telegram chat; no general automation engine.
- [ ] Any new external API key / storage / publishing is treated as T2 — human approval before use.

## Step 1 — Data layer: additive `bot_type` (L9)

1. Add `bot_type TEXT NOT NULL DEFAULT 'shop_bot'` to `bots` in `schema.sql` and `Db::migrate` as an **additive** migration (backfill existing rows to `shop_bot`); no destructive migration.
2. Extend `BotRecord` / `BotConfig` (`src/db.rs:194-204`, `src/config.rs`) with `bot_type`; add `Db` methods for typed listing. No JSON blob for per-type config — dedicated seller-scoped tables later.
3. Add unit tests for migration/backfill and typed listing.
4. Constraints: parameterized SQL only inside `db.rs`; every new record carries `seller_id`; no path/route changes.

## Step 2 — Compile-time capability registry (L9 + L8)

1. Add Cargo features in `Cargo.toml`, e.g. `default = ["shop_bot", "moderation"]`, plus `movie_bot`, `reply_bot`, `file_share`, `web_app`, `service`, `automation`. Keep dependencies for a capability behind its feature so a movie-only build doesn't compile AI/storage deps.
2. Extend `handlers::registry::CommandRegistry` so each capability contributes a `CommandSpec` set + inbound handler; registration is `#[cfg(feature = "...")]` at compile time. No dynamic `.so` loading (deferred non-goal).
3. Dispatcher (`src/main.rs` + `src/handlers/**`) selects the capability set by the receiving bot's `bot_type` through the existing `BotContext` / `BotManager`. Unknown/disabled type → explicit refusal, never silent drop.
4. All existing command paths remain byte-identical in the default build; `git diff --stat -- src/web/mod.rs` stays empty for this step.
5. Verify: `cargo build --no-default-features --features movie_bot` and `cargo build` (all features); `cargo test` for both; record binary sizes and idle-memory smoke check against the <64 MB budget.

## Step 3 — Web selector + gated nav (L7)

1. Generalize `bot_scope_control` / `bot_select` into a Product selector with a Type facet per `IA_MULTIBOT_AMENDMENT_SPEC.md` §3: owner selects + Switch, seller/admin pinned label with `aria-label="Active product"`, buyer absent. Server-side rejection of forged ids/types (403) retained; scope narrows reads only.
2. Add an optional `feature: &'static str` and `product_types` set to `NavItem`; `rail_nav` filters by `cfg(feature)` and runtime type metadata. Existing 27 group rows and Utilities unchanged in the default build.
3. Per-type config: add a `product type` field and closed-by-default `<details>` Advanced disclosure to existing `/bots` pages; no new route for existing configuration.
4. Mobile 375px: stacked label/select/Switch, ≥44px targets, current group auto-expand, Utilities pinned.
5. Verify: nav table tests, role visibility tests, forged-scope test, four viewports, ARIA/keyboard checks.

## Step 4 — Moderation layer, cross-cutting (L9 + L8 + L7 + L10)

1. **Storage (L9):** seller-scoped blocked-word, allow-list, per-bot override (scoped by `bot_id` AND `seller_id`), appeal-item tables + `Db` methods. All parameterized, all `seller_id`-scoped.
2. **Hooks (L8):** inbound dispatcher check before command/text routing (`src/main.rs` near the plain-text fall-through); outbound at the single `ContentSender::send_text` choke point so posts, broadcasts, AI drafts, product delivery, and admin replies all pass through one gate. Deterministic local matching is the fail-closed floor; optional external-AI classification is an enhancement only, never a replacement. Audit-log every decision with reason.
3. **Web surfaces (L7):** owner "Content policy" page in Workspace beside AI (non-overridable floor); seller "Blocked words" page in Offerings (one "Add a word" action, hide/warn/review, empty/error/appeal states, "Always allow" list); per-bot override in `/bots` Advanced defaulting to "Using your shop's rules".
4. **Bot-side appeal (L8):** end-user "Request review" conversation + reason; feeds the seller/owner review queue. Web console shows only the queue/status, not buyer content.
5. **L10 floor:** review before ship — message-content/user-presence exposure, retention minimization, false-positive appeal path, owner-floor conflict resolution. L10 blocks; owning rung remediates.

## Step 5 — Verification & rollout

1. L7/L8/L9 run their own local gates and report; they do not claim verification.
2. **L3** runs the full gate set (fmt, clippy, test, build, audit, deny, verify-agent-tiers), plus: default build route table byte-identical; movie-only build contains only movie capability; all new records `seller_id`-scoped; forged scope rejected; moderation hooks on both paths; a11y at 375/768/1024/1440; no path regression. Mandatory before any completion claim.
3. Rollout: feature-gated, opt-in capability pages; default build behavior-compatible with the approved IA. Rollback = revert each rung's change; only data change is the additive `bot_type` field.

## Deferred (not in this plan)

AI shop bot (first vertical, after foundation), Movie bot, Admin Reply Bot, AI service, File Share, public Web app, general automation engine. T2 gates for priced capabilities, external keys/storage, data deletion, non-goal amendments, and production ship require human approval.
