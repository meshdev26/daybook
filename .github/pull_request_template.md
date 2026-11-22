## What changed

<!-- Short summary and motivation for the change. -->

## Checklist

- [ ] `make test` is green (fmt, clippy `-D warnings`, tests, audit, deny).
- [ ] All DB writes go through `Db` methods, with business data scoped by `seller_id`.
- [ ] No `unwrap()` in request paths; handlers check roles before acting.
- [ ] Money-path tests updated (property tests for anything touching invoices, grants, or refunds).
- [ ] Docs and the decision log (`AGENT.md`) updated where behavior or a tradeoff changed.
- [ ] Changes to payments, keys, publishing, or deletion are not closed by this PR — those require owner approval.
