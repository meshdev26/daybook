## What changed

<!-- Short summary and motivation for the change. -->

## Checklist

- [ ] `make test` is green (fmt, clippy `-D warnings`, tests, audit, deny, routing contract).
- [ ] All DB writes go through `Db` methods, with business data scoped by `seller_id`.
- [ ] No `unwrap()` in request paths; handlers check roles before acting.
- [ ] Money-path tests updated (property tests for anything touching invoices, grants, or refunds).
- [ ] Docs and the decision log (`AGENT.md`) updated where behavior or a tradeoff changed.
- [ ] No T2 changes (payments, keys, publishing, deletion) are closed by this PR — those require human approval.
