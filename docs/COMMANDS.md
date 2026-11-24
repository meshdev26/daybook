# Command Reference

Role-based access: Owner > Admin > Seller > User. Sellers manage their own data; admins/owner see all.

## Roles
- **User (public)**: `/start`, `/help`, `/menu`, `/plans`, `/shop`, `/buy`, `/paid`, `/redeem`, `/services`, `/book`, `/mybookings`, `/cancelbooking`, `/setup`, `/becomeseller`.
- **Seller**: user commands + channel, plans, codes, subscriptions, crypto, broadcast, AI, bookings confirm/decline, MCP tokens, web dashboard.
- **Admin**: seller commands (global view) + `/sellers`, `/refundapprove`, `/payoutapprove`.
- **Owner**: admin commands + `/addadmin`, `/removeadmin`, `/admins`, `/webusers`.

## Commands by bot type

A bot's product type narrows which commands its inbound updates accept. A
`shop_bot` (the default, including every pre-existing bot) runs the full
command tree below. Every other type is live and runs one command family:

- `service` — `/services`, `/book`, `/serviceadd`, `/slotadd`, `/bookings`,
  `/mybookings`, `/cancelbooking`, `/bookingconfirm`, `/bookingdecline`.
- `web_app` — `/dashboard`, `/becomeseller` (both need `WEB_URL`; the links are
  one-time operator-console claim links).
- `reply_bot` — `/support`, `/tickets`, `/reply`, `/closeticket`, `/escalate`.
- `movie_bot` (opt-in feature) — `/shop`, `/find`, `/productadd`, `/products`,
  `/productdel`, `/buy`, `/paid` (catalogue, search, purchase and delivery).
- `file_share` — `/upload` (as a document caption), `/files`, `/share`,
  `/getfile`.
- `automation` — `/ruleadd`, `/rules`, `/ruledel` (keyword auto-replies).

On a scoped bot, any other command gets a short list of the commands that bot
supports. Plain messages stay silent except on `automation` bots, where they
are matched against keyword rules. All capabilities are live: `shop_bot`,
`moderation`, `service`, `web_app`, `reply_bot`, `file_share` and `automation`
are on by default; `movie_bot` needs `--features movie_bot` because an
out-of-scope web-nav test pins the old default feature set. No type is a
notice-only stub.

## Web dashboard
- `/dashboard` — DM a one-time dashboard sign-in link (valid 15 minutes; opening it lets you set a password and signs you in). Creates the account if missing. Requires `WEB_URL`: when it is unset the bot refuses setup, creates no account and discloses no password.
- `/setwebpassword <new>` — change web dashboard password.

## General
- `/start` — welcome + menu. Buyers get the shop welcome (browse `/shop`, buy with `/buy`); sellers/admins get the operator pointer. (public)
- `/help` — list commands. Plain buyers first get a "For buyers" section (shop, plans, buy, paid, redeem, services, book) before the full list. (public)
- `/menu` — inline menu. (public)

## Admin / broadcasting
- `/broadcast <text>` — admin/owner: all registered users; seller: only their own customers; an empty audience is reported as such.

## Channel
- `/channels` — list known channels.
- `/setchannel @chan|id` — set default target channel.
- `/channel` — show default target channel.
- `/info [@channel]` — channel title, description, member count.
- `/post [@channel] <text>` — post text.
- `/postphoto [@channel] [caption]` — then send a photo to post it. Sellers can use it too, but only for their own channel; with no channel set the bot says to run `/setchannel` first.
- `/edit [@channel] <msg_id> | <text>` — edit posted message.
- `/delete [@channel] <msg_id>` — delete posted message.
- `/pin [@channel] <msg_id>` — pin message.
- `/unpin [@channel]` — unpin all.
- `/unpinmsg [@channel] <msg_id>` — unpin specific.
- `/invite [@channel]` — create invite link.
- `/settitle [@channel] <text>` — set channel title.
- `/setdesc [@channel] <text>` — set channel description.

## Plans & subscriptions
- `/plans` — list plans. Buyers see the active plans of every seller, one line each with `— by @seller` (the legacy seller-0 global namespace is excluded); sellers see their own catalogue, admins the global view.
- `/planadd name | price | asset | days` — add plan.
- `/plandel name` — remove plan.
- `/buy <plan>` — create payment invoice. Payment verification via `/paid <invoice_id>` is a manual step today.
- `/paid <invoice_id>` — manual step today: verify payment and grant access. Distinct replies when verification failed permanently (contact the seller, do not pay again) and when the seller's channel isn't configured yet (delivery happens automatically once they finish setup). The signed webhook at `POST /api/v1/webhooks/cryptopay` only enqueues paid invoices for the background granter, which re-verifies with Crypto Pay before granting; [issue #1](https://github.com/meshdev26/daybook/issues/1) tracks confirming end-to-end webhook verification so this manual step can be retired.
- `/grant user_id | days` — manual grant.
- `/revoke user_id` — revoke access.
- `/subs` — list subscriptions.
- `/expirecheck` — manual trigger to revoke expired subscriptions; expiry is handled automatically by the in-process 6h sweeper (the 30s loop verifies invoice payments).

## Services & bookings
- `/services` — buyers: the public bookable catalogue, one line per service (name, duration, price, `— by @seller`; the legacy seller-0 global namespace is excluded). Sellers/admins keep the management view.
- `/book <service_name>` — request the first available slot. One slot holds one booking and is claimed atomically, so a race loser gets "Slot no longer available." The reply states the slot time, that the booking is **requested — not confirmed**, and that the seller has up to 60 minutes to respond; "No available slots." when none is free.
- `/mybookings` — your bookings: id (a public token), service, time and status, with the seller's response deadline for held rows and the seller name for confirmed ones. (public)
- `/cancelbooking <booking_id>` — cancel one of your own bookings (held or confirmed); the seller is DM'd and the slot is freed. Accepts the token id, the integer id or `#id`. (public)
- `/serviceadd name | price | asset | duration_minutes [| channel]` — add a bookable service (seller).
- `/slotadd service | datetime [| capacity]` — add a slot for one of your services (seller). Datetime must be a future UTC time in RFC3339 or `YYYY-MM-DD HH:MM` form; anything else (or a past time) is rejected. `capacity` is accepted for CLI stability but clamped to 1 — one slot holds exactly one booking.
- `/bookings` (alias `/books`) — list bookings (seller: own; admin/owner: all). Both spellings run the same handler. Buyers get pointed at `/mybookings`.
- `/bookingconfirm <booking_id>` — confirm a booking (seller; admin/owner are pointed at the web console `/bookings` page instead). The buyer is DM'd; an expired hold can still be confirmed while the slot is free — if it was re-taken, the confirm fails with "the slot is already taken."
- `/bookingdecline <booking_id>` — decline a booking (seller; admin/owner via the console). The buyer is DM'd and the slot is freed.

## Activation codes
- `/gencodes plan | count | [days]` — generate codes.
- `/codes` — list codes.
- `/redeem CODE` — redeem code.

## Crypto Pay
- Token setup happens in the web console's `/crypto` page or via the `CRYPTO_PAY_TOKEN` env var, never in chat.
- `/crypto me` — app info.
- `/crypto balance` — balance.
- `/crypto rates` — exchange rates.
- `/crypto invoices [status]` — list invoices.
- `/crypto invoice <amount> <asset> [desc]` — create invoice.
- `/crypto check <amount> <asset>` — create check.

## Bot
- `/setupbot` — configure bot profile/commands. Sets the bot description to: "Telegram shop: digital products, channel memberships, codes and bookings. Browse /shop — buy with /buy."
- `/becomeseller` — onboard as a seller; when `WEB_URL` is set, creates a web dashboard account and DMs a one-time 15-minute setup link instead of a password. With `WEB_URL` unset it refuses setup and creates no account or password.

## Admin (owner-only)
- `/addadmin <user_id> [username]` — add platform admin.
- `/removeadmin <user_id>` — remove platform admin.
- `/admins` — list platform admins.
- `/sellers` — list sellers (admin/owner).
- `/webusers` — list web accounts (owner).
## Products & shop (v1.2)

- `/productadd name | price | asset | kind | delivery | [stock]` — create a product (seller). `kind`: `digital_link` | `digital_file` | `physical`; `delivery` is the link/text or Telegram `file_id`; stock blank = unlimited.
- `/products` — list your products (seller/admin). Dashboard equivalent: **Products** page.
- `/productdel <name>` — deactivate a product.
- `/shop` — public listing of products on sale (`/plans` covers subscription plans); buyer lines carry `— by @seller` and the legacy seller-0 global namespace is excluded.
- `/buy <name>` — buy a product or plan by exact name; returns a Crypto Pay invoice. Ambiguous names (two sellers) are refused — ask the seller.
- Delivery: after verified payment the buyer receives the payload as a private message (document re-sent for `digital_file`).

## Movie catalogue (`movie_bot`)

- `/find <text>` — substring search over active, publicly sellable products by name or kind. Results show name, price, asset, medium and seller; delivery payloads are never shown.
- `/shop`, `/productadd`, `/products`, `/productdel`, `/buy`, `/paid` — the shared shop commands described above. On a movie bot they are the catalogue and purchase flow: sellers list products, buyers browse, buy and claim. Verified payment re-sends a `digital_file` payload as a Telegram document. There is no video player or streaming.

## File sharing (`file_share`)

- `/upload <name> [| description]` — attach a document with this caption to register a share. Only the Telegram `file_id` is stored; the bytes stay in Telegram. Sellers and admins only. A text-only `/upload` prints the usage hint.
- `/files` — sellers see their shares on this bot (id and token), admins/owner see all, buyers see only shares explicitly granted to them.
- `/share <id> <user_id>` — grant one user access to one of your shares (seller/admin).
- `/getfile <token>` — re-send the stored document. Access requires ownership, admin/owner, or an explicit grant; a denied attempt is audited. Every upload, grant, denial and download is audit-logged. This is a share flow for Telegram-hosted files, not arbitrary file hosting.

## Automation (`automation`)

- `/ruleadd keyword | response` — create a keyword auto-reply rule scoped to your seller account and this bot (seller/admin). Keywords are matched case-insensitively as substrings.
- `/rules` — list your rules with id, keyword and state.
- `/ruledel <id>` — disable one of your rules; the row is kept for audit.
- Plain-text messages matching an active rule receive the configured response, sent through the outbound moderation gate. At most three rules fire per message and each firing writes an `automation.fire` audit row. There are no schedules, conditions or multi-step actions.

## Finance (v1.2, owner approval for money movement; seller/admin may view and request)

- Refunds: create (seller, from a paid invoice) → approve → execute (owner only for all money movement, with creator separation: the creator may not approve or execute their own refund, `403 creator_forbidden`; non-owners get `403 owner_required`). Execute performs a Crypto Pay transfer; only a FULL refund revokes the grant and kicks the buyer (a partial refund leaves the invoice `paid` with no grant revocation or kick); every step audited.
- Payouts: create (seller) → approve → execute (owner only; transfers to destination).
- `/refundapprove [id]` — owner-only flow via bot: with no id it lists pending refunds (max 10); with an id it approves the refund. Creator separation: the creator may not approve their own refund (`403 creator_forbidden`), and non-owners get `403 owner_required`. State machine mirrors the REST approve endpoint; every approval is audit-logged. A partial refund leaves the invoice `paid` with no grant revocation or kick.
- `/payoutapprove [id]` — same for payouts (max 10 listed, approve by id, audit-logged); refused if the approver's own seller account would benefit.
- The bot never moves money: execution stays in the web console Finance page or the REST execute endpoint.
- API surface: `/api/v1/products`, `/api/v1/shop`, `/api/v1/finance/refunds|payouts/{id}/approve|execute`.

## Physical orders (v1.3)

- A `physical` product purchase creates an order automatically after verified payment.
- `/orders` — list orders (seller: own; admin/owner: all).
- `/orderset <order_id> <status> [tracking]` — seller updates status (`pending|shipped|delivered|refunded|cancelled`) and optional tracking number.

## AI-assisted posting (v1.3.x)
- `/ai <template> | key=val | ...` — generate AI content (admin only; seller access is future — the per-seller `daily_token_budget` override already exists in the data model). Returns the generated text directly.
- `/aipost [@channel] <template> [| key=val ...]` — generate an AI post draft, preview with Publish/Edit/Cancel buttons. Edit sends new text; publish posts to the channel. MCP tool `ai_publish_post` (sensitive, approval-gated) publishes a stored draft by `draft_id`.

## Scheduling (v1.3.x)
- `/schedule [@channel] <datetime> | <text>` — schedule a text post for later delivery. Datetime formats: RFC3339, `YYYY-MM-DD HH:MM`, or `YYYY-MM-DD` (UTC; date-only means midnight UTC). An unparseable datetime is refused with the format spelled out and nothing is stored.
- `/scheduled` — list pending scheduled posts, one line each (id, scheduled UTC time, first ~40 chars of text). Sellers see their own; admins/owner see all.
- `/cancelpost <id>` — cancel one pending scheduled post before it fires (accepts `id` or `#id`). Sellers can cancel only their own; admins/owner any. An unknown id and an already-sent row get the same refusal ("scheduled post #N not found or already sent"); cancelled posts never fire and disappear from `/scheduled`.

## MCP approvals (owner-only, v1.3.x)
- `/approvals` — list pending MCP tool approvals (oldest first).
- `/approve <id>` — approve and execute a pending MCP tool call (owner only; 15-min expiry).
- `/deny <id>` — deny a pending MCP tool call (owner only).

## MCP tokens (v1.4, seller self-service)
- `/mcptoken create <name>` — mint a per-seller MCP bearer token. The raw token is shown once with a store-it-now warning; the reply carries the 8-char prefix and the name. Capped at 10 active tokens per seller (revoke one to free a slot).
- `/mcptoken list` — your tokens as `id | prefix | name | created | status` — prefix-masked, never the raw token.
- `/mcptoken revoke <id>` — revoke one of your own tokens (immediate: clients using it start getting 401s). An unknown id and another seller's id get the same refusal.
- Admins/owners manage only their own tokens here — there is no admin-mints-for-seller flow. Tokens never expire unless revoked.

## MCP tools (AI agents)
Endpoint: `POST /api/v1/mcp`. Bearer auth: the owner's `MCP_OWNER_TOKEN` (disabled when unset) or a per-seller token minted with `/mcptoken` — seller tokens see only their own data (see `docs/API.md` → MCP for the exact auth model). Tool tiers:
- Read: `list_channels`, `list_plans`, `list_posts`, `subs_report`, `list_codes` (PII/codes masked by default).
- Write: `publish_post`, `edit_post`.
- Sensitive (owner-only, approval-gated): `gen_codes`, `delete_post`, `broadcast`, `ai_publish_post`. A non-owner principal calling any of them is refused with JSON-RPC error `-32001 "sensitive tools require the owner credential"` — nothing parks, nothing executes; the owner's park → `/approve` → execute flow is unchanged.
