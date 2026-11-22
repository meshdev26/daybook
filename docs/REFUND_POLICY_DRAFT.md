# Refund Policy — DRAFT (needs legal + L5 review)

> Not in force. Describes the behavior actually implemented in this release;
> L5 rulings are recorded in `docs/MONEY_SAFETY_SPEC.md`.

## Eligibility
A refund can only be created against an invoice verified as paid. Refund
amounts are clamped to the invoice amount, in the invoice's asset, and the
cumulative total of in-flight and completed refunds cannot exceed it.

## Approval
Refunds are **owner-only** to approve and execute. Sellers and admins may
request one but cannot approve or execute it; the creator never approves their
own refund. Transfers carry an idempotency key so a retried call cannot pay
twice.

## Full vs partial
- **Full refund**: the invoice becomes `refunded`, the granted subscription or
  product grant is revoked, and the buyer is removed from the channel.
- **Partial refund**: the invoice stays `paid`; no grant is revoked and no kick
  occurs.

## Timing
No response or payout SLA is promised in this release. Internal recovery timers
(paid-but-ungranted, reconciliation) are operational targets, not commitments.

## How to request
Contact the operator of the instance where you made the purchase. They decide
on the request and execute the refund through the console.
