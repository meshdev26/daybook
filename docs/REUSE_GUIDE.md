# Reuse guide (self-hosting / forking for a different business)

## 1. Scope

This guide is design guidance for operators who want to self-host this software,
or fork it for a different business, and want to know which parts are safe to
reuse and which parts they must replace themselves.

It is **not**:

- a licence grant or a change to the licence (MIT already permits forks);
- an audit, a pentest, or a security certification;
- a claim that the adapter seams described here exist today.

MIT already allows forks: use, copy, modify, merge, publish, distribute,
sublicense and sell are permitted subject to one condition -- the copyright
notice and permission notice must be included in all copies or substantial
portions of the Software (`LICENSE:12-13`). No trademark or logo rights are
granted; do not present a fork as the maintainer's product.

The statements in `docs/LEGAL.md:3-5` and `AGENT.md:158-160` ("free MIT library,
not a hosted or sold service; no ToS/refund/EAA owed") are true of the
**maintainer only**. An operator who sells access or handles customer money owns
their own legal obligations.

## 2. What stays

Reuse these unchanged; they are the hardened core. Do not weaken them to fit a
new business model.

- **Tenant model.** Role-based tenant scoping and the seller/admin/owner/buyer
  distinctions (`src/roles.rs:149`, `Role::tenant_scope`). A seller without a
  binding is `Denied`, not widened to global.
- **Money-safety invariants.** The DB trigger refusing `paid -> failed`
  (`schema.sql:53-57`) and the claim-before-verify, grant-exactly-once flow
  (`src/services/grant.rs:4-7`). Verification errors leave the invoice granted
  by nobody and bounded by an attempt cap.
- **Owner approval.** Sensitive MCP tools never execute without the owner's
  `/approve` in the bot (`ARCHITECTURE.md:34-36`).

Why: these are the parts that have been hardened against tenant leakage,
payment claims and unauthorized sensitive actions. Replacing them removes the
guarantees that make reuse reasonable.

## 3. What must be adapted

Four provider seams. "Current wiring" is what is hard-wired *today*; "seam" is
what a fork would need to introduce. As of this branch, **only the identity
seam exists**, in `src/adapters.rs`, and it is additive: no handler calls it yet,
so behavior is unchanged. The other three are **not implemented**: no trait, no
adapter, no configurable provider exists for them here.

| Seam | Current wiring (hard-coded today) | Seam a fork must add |
|---|---|---|
| Identity / principal | `src/web/mod.rs:159` (`pub identity: IdentityHandle`), with role resolution at `src/roles.rs:149` | `PrincipalProvider`: resolve "who is calling" from the fork's identity source instead of the built-in Telegram identity + `Role` enum. Trait and default provider exist; no call site uses them yet. |
| Payment rail | `src/services/grant.rs:24` (`InvoiceVerifier`, currently `crypto::CryptoPay`) | `PaymentProvider`: extend/replace `InvoiceVerifier` so verification, refunds and asset handling use the fork's rail. Not implemented. |
| Delivery / transport | `src/web/mod.rs:155` (`pub bot: Bot`) | `DeliveryTransport`: send messages and links through the fork's transport instead of the concrete `teloxide::Bot`. Not implemented. |
| Product-type registry | `src/handlers/capabilities/mod.rs:154` (`KNOWN_CAPABILITIES`) | Registry: let a fork add/replace product types and their feature gating without editing the core table. Not implemented. |

Do not assume any row other than identity has code behind it. Treat the table as
a map of where changes would be needed, not a list of shipped interfaces.

## 4. Operator obligations

If you run an instance for others, you own, with counsel:

- your own Terms, Privacy, Refund and Retention documents. The repository files
  are drafts marked "Not in force": `docs/TERMS_DRAFT.md:3-4`,
  `docs/PRIVACY_DRAFT.md:3-5`, `docs/REFUND_POLICY_DRAFT.md:3-4`,
  `docs/RETENTION_DRAFT.md:3-4`. They are templates, not published policy;
- the MIT notice in all copies/substantial portions (`LICENSE:12-13`);
- your own vulnerability-reporting channel;
- an independent penetration test before handling real money. Self-checks are
  explicitly not an audit (`docs/SECURITY.md:43`, `docs/SECURITY.md:47`,
  `docs/SECURITY.md:50`), and isolation must be validated on your deployment.

## 5. Human gates

The following require explicit human owner approval and are not authorized by
this guide or by any code change:

- changing the licence;
- tagging, pushing, publishing a release or announcing in a channel
  (`docs/RELEASING.md:87-89` makes external publishing human-approved and
  human-executed);
- publishing any of the `*_DRAFT.md` documents (each requires legal sign-off);
- moving real money or live customer data.

Nothing in this document grants authority for any of them.

## 6. Honest status

The security posture is alpha and self-reviewed, not independently audited.
From `docs/SECURITY.md:47`, verbatim: "this is not an independent multi-tenant
penetration test or a third-party audit, and the scratch-deployment verification
in issue #1 remains open."

This guide is design guidance, not verification. It does not confirm that
isolation works, that adapters are implemented, or that an instance is fit for
real money.
