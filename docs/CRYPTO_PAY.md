# Crypto Pay Integration

## Setup
- Create app in @CryptoBot → Crypto Pay.
- Set token via `/crypto setup <token>` or `CRYPTO_PAY_TOKEN` env.

## Payment flow
1. `/buy <plan>` creates invoice via `createInvoice`.
2. Bot stores invoice with plan_id and user_id.
3. User pays using pay_url.
4. `/paid <invoice_id>` calls `getInvoices` to verify status.
5. On `paid`, subscription created and invite link sent.

## Methods used
- `getMe`, `getBalance`, `getExchangeRates`
- `createInvoice`, `getInvoices`
- `createCheck`

## Notes
- Always verify payment server-side via API before granting access.
- Invoice status may lag; retry `/paid` if needed.

## Bookings money ruling (L5, 2026-10-27) — governs v1.5

Rules on ARCHITECTURE-V1.4.md §2 Q1 plus adjacent questions. v1.4 ships
free-confirmation unchanged; everything below gates the v1.5 priced-bookings spec only.

1. **Paid-then-unconfirmed → automatic refund in the original asset at the invoiced
   amount. Not credit, not rebook. Design: hold-then-pay.** The invoice is minted at
   `/book` against a live hold, so money only moves against a real slot and the
   seller's `/bookingconfirm` stays the delivery gate. Credit books a platform
   liability against a seller the buyer just lost faith in; rebook demands buyer
   labor to fix a platform failure. For a PAID booking the seller's confirm window
   extends to slot start; unpaid holds expire per v1.4 (D3/D5).

2. **Price is snapshotted at invoice creation (at `/book`); the invoice's amount+asset
   are the authoritative snapshot and the refund basis.** `services.price` is mutable —
   refunding "current price" is undefined after a price change. The refunds table
   (`schema.sql:222-233`) already carries (amount, asset, invoice_id); a refund traces
   to the booking via `bookings.invoice_id` (`schema.sql:397`), so no new schema.

3. **Seller no-show (paid booking never confirmed) = automatic refund *initiation*;
   execution stays owner-side.** The sweeper auto-creates the refund row (`pending`)
   and DMs the buyer — "refund queued" at expiry, "refund sent" on execution.
   Approval/execution remain the existing owner/admin flow (two-person rule intact:
   the benefiting seller is never in the approval path). No code-enforced SLA in v1.5.

4. **Buyer cancels a CONFIRMED paid booking: full auto refund if the cancel is
   ≥ `CANCELLATION_WINDOW_HOURS` (default 24, global constant) before slot start;
   inside the window, no auto refund — owner-discretionary via the existing flow.**
   Binary; no partial refunds in v1.5. Cancelling an unpaid `held` booking is v1.4
   behavior (slot freed, no money). The knob becomes seller-configurable only once
   sellers hold wallet risk (post-payouts).

5. **Bookings inherit and snapshot the service's price+asset at creation — confirmed.
   Mixed-asset revenue *summing* is NOT acceptable, even "until the split ships":**
   `revenue_total` (`db.rs:4380`) already sums `CAST(amount AS REAL)` across assets —
   0.5 BTC + 50 USDT is a meaningless number. Bookings must not extend it; display
   per-asset lines or no total. A cross-asset sum is a defect, not a shortcut.

### Paid-booking state machine (v1.5)

```
/book (buyer, atomic claim):         (none) → held     [slot claimed; invoice minted, price+asset snapshotted]
pay via poller (buyer):              held   → paid     [verified server-side; unpaid expiry = v1.4 'expired']
/bookingconfirm (seller; admin any): paid   → confirmed [terminal; gate closes at slot start; D3 resurrection = unpaid holds only]
/bookingdecline (seller; admin any): held|paid → declined [paid: auto refund row spawned]
sweeper at slot start, paid & unconfirmed: paid → refunded [auto refund row; buyer DM'd]
/cancelbooking buyer /bookings/cancel console:
                                     held → cancelled (no money)
                                     paid → cancelled + auto refund if ≥ window before slot,
                                            else refund request → owner review
```
Terminal: `confirmed`, `cancelled`, `declined`, `refunded`. A `refunded` booking is
terminal at refund spawn; the refunds row tracks pending → approved → processed →
executed separately. Unpaid expiry (`held → expired`) is exactly v1.4 — no money moved.

**Dispatch note:** Option A is **confirmed** as designed — additive
`invoices.booking_id` mirroring the `product_id` precedent, invoice-as-snapshot, no
additional schema. The v1.5 spec must add the refund-on-non-delivery arm to the grant
dispatch, the `CANCELLATION_WINDOW_HOURS` constant, and narrow D3 resurrection to
unpaid holds. Per-asset revenue is the one flagged conflict (ruling 5).