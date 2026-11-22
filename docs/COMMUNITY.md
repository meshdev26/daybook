# Community channel — Daybook

## Decision
Defer a public updates channel until the release gates close. Recorded in
`AGENT.md` §11. Until then `CHANGELOG.md` is the single source of truth for
release news.

## Platform
Telegram: the audience is mobile-first Telegram sellers and the product is
Telegram-native. Announcement-only; no support or feature requests.

Channel: https://t.me/daybook26 (owner-created; not published by this repo).

## Triggers (all required before creating)
- Publishing T2 approved by the owner; version aligned (`0.N.0` ↔ milestone vN).
- Named LICENSE holder; Terms and Privacy published.
- At least one real net-new vertical shipped (six types are currently stubs).

## Scope
Release notes distilled from `CHANGELOG.md`. One designated poster. No support
threads, no commitments on unshipped features, no discussion of customer data.

## Security rules
- No tokens, keys, credentials, customer data, or internal paths in any post.
- No deployment details or live incident specifics.
- Channel moderation on; posters are named and few.
- Creating and publishing the channel is a human T2 action.
