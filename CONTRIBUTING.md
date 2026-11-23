# Contributing

Thanks for improving Daybook. The repo is set up so
the tooling enforces most rules — you just run it.

## Setup

```bash
git clone https://github.com/meshdev26/daybook.git && cd daybook
cargo build          # Rust 1.99+/edition 2024 (rust-toolchain.toml pins it)
make test            # the full gate suite
```

## Pre-push guard

The `.githooks/pre-push` hook enforces the verify-before-push rule from
`AGENT.md` locally: a dirty tracked tree, banned comment tokens, DRAFT or
ANNOUNCEMENT files, and whitespace errors all block the push. It is a fast
guard, not CI; the full gates still run before merge. The hook is opt-in per
clone because git config is never committed, so activate it once after cloning:

```bash
git config core.hooksPath .githooks
```

## The rules that matter

1. **Read before write** — never propose changes to code you haven't read.
2. **All persistence goes through `Db` methods** (parameterized SQL only),
   all business data scoped by `seller_id`.
3. **No `unwrap()` in request paths**; handlers check roles before acting.
4. **Money paths fail closed** and must be exactly-once — if you touch
   invoices/grants/refunds, extend the property tests that guard them.
5. **Gates before merge:** `make test` runs fmt, clippy `-D warnings`, all
   tests, `cargo audit`, and `cargo deny`. A PR that doesn't pass them doesn't
   get reviewed.

**Client JS:** `make js-check` type-checks `assets/app.js` against the DOM
(typescript) and runs its jsdom test. It is dev-only — Node is required and it
is deliberately outside `make test`, which stays Rust-only. The CI
`client-check` job runs the same commands; it is a required status and must be
green before merge.

**Compliance posture:** Daybook is a free MIT library, not a hosted or sold service. The maintainer owes no Terms of Service, refund policy, or EAA statement; each self-hoster owns their instance's legal documents, with templates in `docs/TERMS_DRAFT.md`, `docs/PRIVACY_DRAFT.md`, `docs/REFUND_POLICY_DRAFT.md`, `docs/RETENTION_DRAFT.md`, and `docs/ACCESSIBILITY_DRAFT.md` (see `docs/LEGAL.md`). Contributors should not add fee, AI-charging, or promo value while guards G1–G8 (see `docs/MONEY_SAFETY_SPEC.md`) are deferred.

## Finding your way around

- `ARCHITECTURE.md` — system map: interfaces, core, data flow, and modules.
- `AGENT.md` — coding standards + decision log (the *why* behind the code);
  this is the single agent-instruction file for the repository.
- `docs/ARCHITECTURE-SCALE-PLAN.md` — approved designs and seam contracts;
  don't build a seam ahead of its documented trigger.

## Adding a feature (short version)

1. Schema change → `schema.sql` + additive migration in `Db::migrate` (never
   destructive; existing rows keep behaving).
2. `Db` methods + unit tests (property tests for anything money-adjacent).
3. Wire the surface: bot command (`handlers/commands.rs`), API
   (`src/api.rs`), or web (`src/web.rs` — escape every dynamic value with
   `esc()`, every link via `state.url()`).
4. Docs: `docs/COMMANDS.md` / `docs/API.md` + a decision-log line in
   `AGENT.md` if it changes a tradeoff.
5. `make test` green → PR.

## Reporting a security issue

Please do not open a public issue for vulnerabilities. Report them privately
through GitHub Security Advisories — open the repository's **Security** tab and
choose **Report a vulnerability**. See `docs/SECURITY.md` for the current
guarantees.
