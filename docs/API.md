# REST API Reference

Base URL: `https://<host>/api/v1`. Health check: `GET /health` (no auth).

## Auth
All endpoints except `/health`, `/metrics`, `/api/v1/auth/login`, `/api/v1/auth/mfa/verify` and `/api/v1/webhooks/cryptopay` require authentication via `session` cookie or `Authorization: Bearer <token>`. `/metrics` is deliberately unauthenticated; the server binds `127.0.0.1` by default (`WEB_BIND`), so scrape it locally or protect the port before binding `0.0.0.0`.

Roles: `owner > admin > seller > user`. Sellers are auto-scoped to their own `seller_id`.

### Authentication
| Method | Path | Role | Description |
|--------|------|------|-------------|
| POST | /auth/login | public | `{username, password}` → `{token, expires_at, account_id, role, seller_id}`. TOTP-enabled accounts instead return 200 `{mfa_required: true, pending_token}` — a 5-minute single-use token, not a session; TOTP-disabled owner/admin get 401 `mfa_required` ("enroll via the web console first") |
| POST | /auth/mfa/verify | public (pending token) | `{pending_token, code}` → `{token, expires_at, account_id, role, seller_id}`. `code` is a TOTP authenticator code or a recovery code; the pending token is single-use; 401 `invalid_code` on a bad code; failed attempts throttled 5 per 15 min per pending token (429 `too_many_attempts`) |
| POST | /auth/logout | any | Revoke session |
| GET | /auth/me | any | Current account |
| POST | /auth/password | any | `{new_password}` |
| GET | /auth/api-keys | seller+ | List API keys |
| POST | /auth/api-keys | seller+ | `{name, scopes}` → returns full key once |
| DELETE | /auth/api-keys/{id} | seller+/owner | Revoke key |

Login on a TOTP-enabled account is two-step: `POST /auth/login` never mints a session directly — exchange the returned `pending_token` plus a valid code at `POST /auth/mfa/verify` (REST parity with the web flow). Enrollment itself is web-console only.

### Sellers
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /sellers | admin+ | List sellers |
| GET | /sellers/me | seller+ | Current seller |
| GET | /sellers/{user_id} | self/admin+ | Seller detail |
| POST | /sellers | admin+/self | Upsert seller |
| POST | /sellers/{user_id}/channel | self/admin+ | Set default channel (proof of control for non-admin callers, see below) |
| GET | /sellers/{user_id}/ai-settings | self/admin+ | Read back AI settings |
| POST | /sellers/{user_id}/ai-settings | self/admin+ | Set AI settings (partial update) |

`GET /sellers/{user_id}/ai-settings` → `{user_id, provider, model, daily_token_budget, rpm_limit}` (nulls for unset fields; 404 if the seller does not exist).

`POST /sellers/{user_id}/ai-settings` → `{ok: true}`. Body: `{provider?, model?, daily_token_budget?, rpm_limit?}` — all fields optional; a partial update (omitted fields keep their stored values, and stored values cannot be cleared). Validation: `provider` ∈ {openai, anthropic, ollama, openrouter, custom}; `model` 1–128 chars; `daily_token_budget` and `rpm_limit` ≥ 0. Today only `daily_token_budget` changes runtime behavior (it overrides the global AI budget); `provider`, `model` and `rpm_limit` are stored for future use.

`POST /sellers/{user_id}/channel` — proof of control for non-admin callers: a `@handle` is accepted only when the calling seller is an administrator of that chat (verified live through the bot via `getChatAdministrators`; the bot not being in the channel → 400 `invalid_channel`), a numeric id must be a negative group/channel id, and freeform names are rejected with 400 `invalid_channel`. Admins/owner are exempt (bootstrap runs before the bot is in any chat).

### Plans
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /plans | scoped | List plans |
| POST | /plans | seller+ | Create plan |
| GET | /plans/{id} | scoped | Plan detail |
| PATCH | /plans/{id} | owner/admin/owner-plan | Update plan |
| DELETE | /plans/{id} | owner/admin/owner-plan | Delete plan |
| POST | /plans/{id}/active | owner/admin/owner-plan | `{active}` |

### Subscriptions
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /subscriptions | scoped | List subscriptions |
| POST | /subscriptions | seller+ | Grant subscription |
| GET | /subscriptions/{id} | scoped | Subscription detail |
| PATCH | /subscriptions/{id} | seller+ | Expire/revoke/extend |
| POST | /subscriptions/expire-check | seller+ | Revoke expired |
| GET | /users/{user_id}/subscriptions | self/admin+ | User's subscriptions |

### Invoices
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /invoices | scoped | List invoices |
| POST | /invoices | seller+ | Create invoice |
| GET | /invoices/{invoice_id} | scoped | Invoice detail |
| POST | /invoices/{invoice_id}/status | seller+ | `{status}` |
| POST | /invoices/{invoice_id}/claim | scoped | Verify payment + grant access |
| POST | /webhooks/cryptopay | public (signed) | CryptoPay webhook |

### Codes
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /codes | scoped | List codes |
| POST | /codes | seller+ | Generate codes |
| POST | /codes/reclaim | scoped | Reclaim a code: revokes the old one and issues a replacement copying plan/days/max_uses/expiry. Body `{code, length?}` (length 1–32, default 12) → `{code}`; unknown or foreign code → 404 |
| GET | /codes/{code} | scoped | Code detail |
| DELETE | /codes/{code} | scoped | Delete code |
| GET | /codes/{code}/redemptions | scoped | Redemptions |
| POST | /codes/{code}/redeem | auth | Redeem code |

`GET /codes/{code}/redemptions` — redemption history for one code, newest first. Sellers see only their own codes' redemptions (a foreign code is a 403, never an empty 200); admin/owner see all. Hard cap of 200 rows, with no truncation flag. Response: `{items: [{id, code, user_id, redeemed_at}], total}`.

### Posts
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /posts | scoped | List posts |
| POST | /posts | seller+ | Send + log post |
| POST | /posts/schedule | auth | Schedule a post: `{channel?, text, scheduled_at}` → 201 `{id, scheduled_at}`; `channel` defaults to the global default channel; stored under the caller's `seller_id` |
| GET | /posts/{id} | scoped | Post detail |
| PATCH | /posts/{id} | seller+ | Edit post |
| DELETE | /posts/{id} | seller+ | Delete post |
| POST | /posts/{id}/pin | seller+ | Pin message |
| POST | /posts/{id}/unpin | seller+ | Unpin message |
| POST | /broadcasts | seller+ | `{text}` scoped broadcast |

### Users
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /users | scoped | List users |
| GET | /users/{user_id} | self/admin+ | User detail |
| POST | /users/upsert | seller+ | Upsert user |

### Stats & Config
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /dashboard/summary | scoped | KPIs |
| GET | /revenue | scoped | Revenue total |
| GET | /channels | scoped | Known channels |
| GET | /channels/{id} | scoped | Channel info |
| PATCH | /channels/{id} | seller+ | Update channel |
| GET | /config/channel | auth | Default channel |
| POST | /config/channel | seller+ | Set channel |

### AI
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /ai/templates | seller+ | List templates |
| POST | /ai/templates | seller+ | Create template |
| GET | /ai/templates/{name} | seller+ | Template detail |
| PATCH | /ai/templates/{name} | seller+ | Update template |
| DELETE | /ai/templates/{name} | seller+ | Delete template |
| GET | /ai/usage | seller+ | Usage summary |

### Finance
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /finance/summary | scoped | Revenue summary |
| GET | /finance/revenue-by-seller | admin+ | Revenue by seller |
| GET | /finance/payouts | scoped | List payouts |
| POST | /finance/payouts | seller+ | Create payout |
| PATCH | /finance/payouts/{id} | owner | Update payout — only transition permitted is approved → failed (409 otherwise) |
| GET | /finance/refunds | scoped | List refunds |
| POST | /finance/refunds | seller+ | Create refund |
| GET | /finance/export | scoped | CSV export |

### Admin
| Method | Path | Role | Description |
|--------|------|------|-------------|
| GET | /admin/sellers | admin+ | List sellers |
| GET | /admin/sellers/{user_id} | admin+ | Seller detail |
| PATCH | /admin/sellers/{user_id} | admin+ | Update seller |
| GET | /admin/stats/global | admin+ | Global stats |
| GET | /admin/admins | owner | List admins |
| POST | /admin/admins | owner | Add admin |
| DELETE | /admin/admins/{user_id} | owner | Remove admin |
| GET | /admin/web-accounts | owner | List web accounts |
| POST | /admin/web-accounts | owner | Create web account |
| DELETE | /admin/web-accounts/{id} | owner | Delete web account |
| PATCH | /admin/web-accounts/{id}/password | owner | Set password |
| GET | /admin/audit-log | admin+ | Audit log |

## Error format
```json
{ "error": { "code": "not_found", "message": "invoice not found" } }
```

## Pagination
`?limit=50&offset=0` → `{ "items": [...], "total": N }`
---

## Products & Shop (added v1.2)

Auth: session cookie (web) or `Authorization: Bearer <api-key>` where noted; JSON bodies.

| Method & path | Role | Description |
|---|---|---|
| `GET /api/v1/shop` | public | Active products for sale (no delivery payloads). |
| `GET /api/v1/products` | seller (own) / admin (all) | List products incl. inactive. |
| `POST /api/v1/products` | seller | Create: `{name, price, asset, kind: digital_link|digital_file|physical, delivery?, stock?}`. Name collisions (plan or product, same seller) → 400. |
| `DELETE /api/v1/products/{name}` | seller/admin | Deactivate. |
| `POST /api/v1/invoices/{id}/claim` | buyer (invoice owner) / admin | Verify via Crypto Pay (fail-closed) → grant + DM delivery + finalize. Product invoices grant once (UNIQUE); out-of-stock finalizes without grant. |

## Finance (added v1.2)

Money movement is owner-approved (T2) and audited (`audit_log`).

| Method & path | Role | Description |
|---|---|---|
| `POST /api/v1/finance/refunds` | seller | Create refund request from one of their paid invoices (amount/asset default to the invoice's). Seller callers: `amount` must parse as a number (400 otherwise), is clamped to the invoice amount, and `asset` must equal the invoice asset (400 `invalid_asset`). |
| `POST /api/v1/finance/refunds/{id}/approve` | admin/owner | pending → approved. |
| `POST /api/v1/finance/refunds/{id}/process` | admin/owner | approved → processed (no transfer): invoice marked refunded, buyer kicked, tied subscriptions expired. 409 unless status is `approved`. |
| `POST /api/v1/finance/refunds/{id}/execute` | admin/owner | Executes Crypto Pay `transfer` to the invoice's buyer; on success: executed, invoice refunded, subscription revoked + buyer kicked. |
| `POST /api/v1/finance/payouts` | seller | Create payout request `{amount, asset, destination?}` (admins must name `seller_id`). |
| `POST /api/v1/finance/payouts/{id}/process` | owner | approved → paid (manual mark-as-paid outside Crypto Pay). 409 unless status is `approved`. |
| `POST /api/v1/finance/payouts/{id}/approve` / `{id}/execute` | owner only | Approve, then execute the transfer (destination must be the Telegram user id to pay). |
| `PATCH /api/v1/finance/payouts/{id}` | owner | Only transition permitted is approved → failed; anything else is 409. |

Both `/execute` endpoints are guarded by a CAS `executing` state: the row is first atomically claimed approved → executing (a losing concurrent caller gets 409 "execution already in progress"), and only then does the transfer run. A definite Crypto Pay rejection reverts the row to `approved` and returns 502 `transfer_failed` (retryable); an ambiguous failure (timeout/network/5xx — money state unknown) returns 503 `execution_ambiguous` and deliberately leaves the row `executing`: reconcile on the console Finance page before retrying. `payout/refund_execute_attempt` and `payout/refund_execute_ambiguous` audit events cover both paths.

Manual `"paid"` transitions on invoices are rejected (400) — payment state changes only through the verified webhook/claim paths.

## MCP (added v1.3)

JSON-RPC 2.0 over HTTP: `POST /api/v1/mcp` (GET → 405). Stateless — one response per request, no session id; notifications (no `id`) get `202` + empty body. Every `tools/call` outcome (ok or err, every tier, unknown tools included) lands in the `mcp_audit` table.

### Auth
Bearer only, independent of REST auth. Two credential classes, tried in order:

1. **Owner env token** — `Authorization: Bearer <MCP_OWNER_TOKEN>`, constant-time compared; maps to the **Owner** principal (back-compat: the v1.3 flow, unchanged).
2. **Per-seller tokens (v1.4)** — any other bearer is looked up by its SHA-256 hash in `mcp_tokens`; a live row (not revoked, not expired) maps to a **Seller** principal bound to that seller id, followed by a best-effort `last_used_at` touch. Tokens are minted by the seller in the bot (`/mcptoken create <name>`), shown once, stored SHA-256-hashed with an 8-char display prefix, self-revocable via `/mcptoken revoke <id>`, capped at 10 active per seller, and never expire unless revoked. Sellers can only ever act on their own data: every read tool scopes its results by the token's seller id (the scope comes from the principal, never from `seller_id` arguments), and rows a seller creates are attributed to them.

Secure default OFF: with `MCP_OWNER_TOKEN` unset or empty the endpoint refuses everything with 403 `mcp disabled` — the identical response for any token, so nothing hints a valid one exists (per-seller tokens alone do not enable the endpoint). Unknown / revoked / expired / malformed bearers all get the same 401 — no oracle in the failure path.

| Condition | Response |
|---|---|
| `MCP_OWNER_TOKEN` unset/empty | `403` `-32000 "mcp disabled"` — for any caller |
| Missing/wrong bearer (incl. revoked or expired per-seller tokens) | `401` `-32000 "unauthorized"` |
| > 60 requests/min (token bucket per principal — one bucket per seller id; the owner keeps one) | `429` `-32000 "rate limited, retry in <n>s"` |

### Methods
| Method | Result |
|---|---|
| `initialize` | Echoes the client's `protocolVersion` when it is a known `2024-*`/`2025-*`, else answers with the `2025-06-18` baseline; `capabilities: {tools: {listChanged: false}}`; `serverInfo: {name: "telegram-agent", version}` |
| `ping` | `{}` |
| `tools/list` | `{tools: [{name, description, inputSchema, annotations}]}` |
| `tools/call` | `{name, arguments}` → `{content: [{type: "text", text: "<tool result as JSON>"}], isError}` — tool errors live in the result (`isError: true`), protocol errors in `error` |

Protocol errors: unparseable body → HTTP 400 `-32700` (`id: null`); unknown/missing method → `-32601`; unknown tool, missing name, or invalid arguments → HTTP 200 `-32602`. Arguments are validated before anything runs, so a sensitive tool never parks an approval for bad args.

```json
// initialize
→ {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}
← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":false}},
   "serverInfo":{"name":"telegram-agent","version":"0.1.0"}}}

// tools/call of a Sensitive tool: nothing executes, an approval is parked
→ {"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"gen_codes","arguments":{"count":3,"days":30}}}
← {"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"{\"requires_approval\":true,\"approval_id\":7,
   \"expires_at\":\"2026-10-27T12:30:00+00:00\",\"how\":\"approve via /approve <id> in the bot (owner only)\"}"}],"isError":false}}

// tools/call of a Sensitive tool with a per-seller token: refused before
// anything parks — no approval row, no execution, not even arg validation
→ {"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"gen_codes","arguments":{"count":3,"days":30}}}
← {"jsonrpc":"2.0","id":3,"error":{"code":-32001,"message":"sensitive tools require the owner credential"}}
```

### Tools
| Tool | Tier | Purpose |
|---|---|---|
| `list_channels` | Read | Default target channel + each seller's channel |
| `list_plans` | Read | List subscription plans |
| `list_posts` | Read | Recent stored posts, newest first (`limit` default 20, max 50) |
| `subs_report` | Read | Subscription counts by status; user ids masked unless `include_user_ids: true` (response then flagged `contains_pii: true`) |
| `list_codes` | Read | Recent activation codes, always masked to the first 4 chars + `…` |
| `publish_post` | Write | Send + log a text post (default channel unless `channel` given) |
| `edit_post` | Write | Edit a stored post record (the already-sent Telegram message is not rewritten) |
| `gen_codes` | Sensitive | Generate activation codes (`count` 1–50); full codes go to the owner's Telegram chat only, never the MCP response |
| `delete_post` | Sensitive | Delete a stored post record (the sent Telegram message is not unsent) |
| `broadcast` | Sensitive | Text message to all known users |
| `ai_publish_post` | Sensitive | Publish an AI-generated post draft by `draft_id` |

Read and Write tools execute on the call (`readOnlyHint` true/false respectively); `list_codes` and `subs_report` mask secrets/PII by default. **Sensitive tools are owner-only and never execute on the call path**: a per-seller principal calling any Sensitive tool is refused with JSON-RPC error `-32001 "sensitive tools require the owner credential"` (HTTP 200) before anything parks — no approval row, no execution, not even arg validation (the gate sits before the validator). The owner flow is unchanged: decide via `/approve <id>` / `/deny <id>` in the bot within 15 minutes of `expires_at` (expired rows are decided to `expired`, never executed; the atomic pending→decided flip makes a double `/approve` execute once; approved work re-runs the tool's own validator and executor). `/approvals` lists pending rows.
