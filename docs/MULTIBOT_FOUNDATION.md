# Multi-Bot Foundation

Daybook is the calm back-office ledger these products live in.

One process, one database, one console — several bots and several kinds of
product. This guide explains how products are typed, how the console scope
selector works, how a build registers only the capabilities you
offer, and how message moderation is configured.

## Product types

Every product carries a **type**. The type is metadata: it groups products in
the console and tells the dispatcher which capability a bot belongs to. It does
not create a separate console, login, or port.

| Type | Plain-language name | What it is |
|------|---------------------|------------|
| `shop_bot` | Shop bots | The AI shop bot — subscriptions, products, payments (the existing default). |
| `movie_bot` | Movie bots | Movie catalog bot (deferred — stub, notice only). |
| `reply_bot` | Reply bots | Admin Reply Bot (deferred — stub, notice only). |
| `web_app` | Web apps | Public Web app (a non-bot surface; deferred — stub, notice only). |
| `service` | Services | AI service (deferred — stub, notice only). |
| `file_share` | File sharing | File Share (deferred — stub, notice only). |
| `automation` | Automation | Automation rules (deferred — stub, notice only). |

A bot with no type set — including every bot created before this foundation —
is treated as a `shop_bot`. Leaving the type blank keeps that behavior.

Moderation is **not** a product type. It is a cross-cutting layer that applies
to products carrying messages (see [Moderation](#moderation)).

## The Product / Type selector

The console rail has a scope control just below the brand block. It reads scope
first, before any page content.

- **Owner** — sees a **Product** selector, a **Product type** facet, and a
  **Switch** button. Products are grouped under type headings (for example,
  "Shop bots", "Movie bots"). Bot options are labelled by name and, when set,
  `@username`. **All products** (the empty selection) means the unscoped global
  view — it is not a filter for one "main" bot. The Product type facet is a
  convenience for narrowing the list; it does not submit, save, or grant
  anything, and it can be ignored without losing state.
- **Seller and admin** — see a pinned **Active product** label naming their
  product. Their product is resolved by the server and cannot be switched; a
  forged switch request is refused.
- **Buyer** — sees no scope control and no moderation settings. Their view is
  their own purchases and orders, as before.

Selecting a product narrows what you read. It never changes what you are
allowed to do.

## Capability features and builds

Capabilities are chosen at compile time. Each one is a Cargo feature that controls what the binary **registers and accepts at runtime**: only compiled-in capabilities are registered, and a bot whose type was not compiled in is explicitly refused (`capability_uncompiled`). Features do not trim dependencies or binary size — see "Scope of gating" below.

| Feature | Enables |
|---------|---------|
| `shop_bot` | Shop bot — payments, products, AI drafts. |
| `moderation` | Owner / seller / per-bot word policy floor. |
| `movie_bot` | Movie catalog bot (deferred — stub, notice only). |
| `reply_bot` | Admin Reply Bot (deferred — stub, notice only). |
| `file_share` | File Share product type (deferred — stub, notice only). |
| `web_app` | Public Web app surface (deferred — stub, notice only). |
| `service` | AI service surface (deferred — stub, notice only). |
| `automation` | Automation rules (deferred — stub, notice only). |

The default build enables `shop_bot` and `moderation` and keeps every existing path unchanged. To build a movie-only binary that registers only the movie capability (plus the cross-cutting moderation floor) and refuses every other bot type, including the shop bot:

```bash
cargo build --no-default-features --features movie_bot
```

**Scope of gating (what is NOT reduced).** These features are registration switches, not dependency trims. Every dependency in `Cargo.toml` is still compiled and linked in a movie-only build — the console/web stack (axum, reqwest), the MTProto identity stack (grammers, aes-gcm, pbkdf2), and the shop/AI/payment paths all remain in the artifact; `cargo tree --depth 1` is identical to the default build and the binary is not smaller. Removing unused dependencies first requires gating the shared web/api/auth/identity/AI/shop modules (web + server) and then marking the Cargo dependencies optional (core). That cross-cutting refactor is a deferred follow-up, outside this foundation's approved scope. It does not weaken the runtime guarantee above: unregistered bot types are always refused.

Build the other narrow installs the same way, naming the features you want.
Capabilities are fixed at build time: adding or removing one means rebuilding
and redeploying the binary. Loading capabilities dynamically at runtime
(plugins) is a non-goal.

## Moderation

Moderation is a cross-cutting configuration layer, not a standalone bot. Three
levels stack, from broadest to most local:

1. **Owner global floor — "Content policy"** (Workspace, beside AI, owner only).
   This is the non-overridable baseline: a minimum enforcement mode and a set of
   baseline terms. Seller and per-bot rules may add terms or raise enforcement,
   but can never loosen a baseline term or fall below the floor.
2. **Seller rules — "Blocked words"** (Offerings). The main day-to-day surface,
   with a single primary action, **Add a word**. Rules here add to the floor for
   that seller and may be stricter, never looser. Each word has a mode and a
   status.
3. **Per-bot override** (Advanced on a bot's settings). Defaults to **Using your
   shop's rules**, which inherits the seller's Blocked words. Choosing **Custom
   rules for this bot** reveals a word list for that bot only; those rules can
   add to the seller and floor layers but never weaken them. Returning to
   **Using your shop's rules** restores inheritance.

**Modes** are the same plain-language three at every level:

- **hide** — matching messages are hidden.
- **warn** — a warning is shown.
- **review** — matching messages are sent for review.

From least to most strict the order is warn, review, hide. When layers overlap,
the strictest mode wins, and a floor term can never be weakened.

**Always allow.** A separate "Always allow" list on the seller page holds terms
that are exempt from filtering for that seller. It takes precedence over that
seller's Blocked words, but it cannot cancel a floor term.

**Appeals.** Appealed items appear in a review list with a **Request review**
action, a reason field, and a status: **Pending**, **Approved**, or **Upheld**.
Status is shown in text (with a dot), never by colour alone. The buyer-facing
appeal conversation happens in the bot; the console shows only the review queue
and its status.

**Audit trail.** Moderation decisions are logged with a short reason or reason
code and an opaque message reference. Message text is never stored in the
audit trail.

## Before you enable moderation

The owner global floor is **seeded empty** so that existing installs behave
exactly as before. A fresh or upgraded install therefore has no baseline terms
until an owner adds them.

Set at least one floor term on the owner **Content policy** page before turning
on moderation enforcement. With an empty floor there is no non-overridable
baseline, so enforcement cannot be relied on.

Moderation is fail-closed. If the moderation storage is not ready or the floor
cannot be read, enforcement is marked **degraded**: hooks hold matching
messages for review instead of treating the incomplete policy as "nothing to
do". A degraded install never silently lets messages through.

## Non-goals

- No machine-learning training or local models — external AI API calls only.
- No public non-Telegram chat.
- No general automation engine.
- No dynamic plugin (`.so`) loading.
