# Contributing

Thanks for improving the Telegram Business Platform. The repo is set up so
the tooling enforces most rules — you just run it.

## Setup

```bash
git clone <repo> && cd telegram-agent
cargo build          # Rust 1.99+/edition 2024 (rust-toolchain.toml pins it)
make test            # the full gate suite
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

**Compliance posture:** Terms of Service, Privacy, Refund, and Retention policies and an EAA statement are pending before any paid public launch; contributors should not add fee, AI-charging, or promo value while guards G1–G8 (see `docs/MONEY_SAFETY_SPEC.md`) are deferred.

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

Please do not open a public issue for vulnerabilities. A private reporting
channel will be enabled before the public launch; until then, see
`docs/SECURITY.md` for the current guarantees.
