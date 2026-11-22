# Privacy Policy — DRAFT (needs legal review)

> Not in force. Human/legal sign-off required before publication. Describes the
> data this codebase actually stores and sends; verify against a deployment
> before publishing.

## What is stored
- **Telegram identity**: user id, and username where supplied, for sellers,
  customers, orders, grants, subscriptions and bookings (`seller_id`-scoped).
- **Web accounts**: username, salted password hash, role, optional seller id.
  Passwords are never stored in plain text; sessions are stored as hashes.
- **Commerce records**: invoices, payments, refunds, payouts, codes, promos,
  referrals, bonus balances, with amounts and timestamps.
- **Content**: posts and scheduled posts you create; moderation policy terms
  and opaque decision references. **Message bodies are not stored** by the
  moderation layer — only an opaque hash reference, a matched policy term, an
  action code and a short reason.
- **Preferences**: theme and navigation state are kept in the browser only.

## Who controls it
The operator of each instance controls its database. Data is scoped by
`seller_id`; the owner has global visibility. Multiple sellers' data is
isolated within one instance.

## Third parties
- **Crypto Pay** receives invoice and transfer requests to verify payments and
  move funds.
- **AI providers** (OpenAI / Anthropic, if enabled by the operator) receive
  generation prompts and are configured by the operator; no customer data is
  sent by default.
- No analytics or advertising trackers are built in.

## Your rights
Buyers may request a data export or deletion through the operator (`/privacy`);
the operator handles the request. Sellers can export and delete customer
records through the console. Retention periods are not yet set (see
`docs/LEGAL.md`).

## Security
HTTPS is expected at the reverse proxy; cookies are HttpOnly and SameSite;
webhook signatures are verified. Report vulnerabilities privately — see
`docs/SECURITY.md`.
