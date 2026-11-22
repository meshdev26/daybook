# Data Retention — DRAFT (needs owner decision + legal review)

> Not in force. The 7-year figure below is the recommended default, not yet
> ruled by the owner. No retention promise is made until that ruling exists.

## Proposed schedule
| Record class | Retention | Basis |
|---|---|---|
| Invoices, refunds, payouts, audit log | 7 years from last movement | Financial/accounting record |
| Orders, grants, subscriptions, bookings | 7 years from closure | Tied to a financial record |
| AI usage/cost, bonus/referral/promo ledgers | 7 years | Accounting trail |
| Moderation decisions (opaque refs, no message bodies) | 12 months | Operational need only |
| Web sessions | 7 days expiry | Security |
| Buyer customer profile/tags/notes | Until deletion request | CRM convenience |

## Rules
- Money rows are append-only; no self-service deletion of financial records.
- Backups inherit the same schedule.
- Export is owner-only and audited (`docs/API.md`); no export SLA is promised.
- A buyer deletion request is handled by the operator (`/privacy`).

## Open decision
Confirm the 7-year figure and the 12-month moderation window, or replace them.
Record the ruling here and in `AGENT.md` §11.
