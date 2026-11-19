//! MCP tool registry: the tool table, its three permission tiers, and the
//! handlers behind them.
//!
//! The tier model is the permission system:
//! - *Read* tools are direct, annotated `readOnlyHint: true`, and mask any
//!   secret-bearing field (codes) or PII (subscriber user ids) by default.
//! - *Write* tools execute immediately (mirroring the web API's behavior) and
//!   are audited like everything else.
//! - *Sensitive* tools NEVER execute on the MCP call path. Calling one parks
//!   a pending approval row and returns `requires_approval`; the actual work
//!   runs only from the owner's `/approve` via [`execute_sensitive`], which
//!   goes through the same validate + execute functions the call path would
//!   have used. Generated codes therefore reach the owner's Telegram chat and
//!   nothing else — never the AI transcript, never `mcp_audit`.
//!
//! Identity read tools ([`IDENTITY_TOOLS`], the L10 owner-principal-only
//! set) are Read-tier but additionally refuse every principal that carries a
//! seller id: they operate the operator's personal Telegram account, which a
//! per-seller token never owns.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};
use teloxide::payloads::SendMessageSetters;
use teloxide::prelude::Requester;

use crate::roles;
use crate::web::WebState;

/// How long a parked approval stays actionable. The owner must decide within
/// this window or the row expires (purged lazily on list).
const APPROVAL_TTL_SECS: i64 = 15 * 60;

/// Upper bound for `gen_codes` count / list-tool limits — matches the bot's
/// `/gencodes` cap so no interface can mint unbounded code bursts.
pub const MAX_CODES: i64 = 50;
pub const DEFAULT_LIST_LIMIT: i64 = 20;
pub const MAX_LIST_LIMIT: i64 = 50;
/// How far back `edit_post` scans a seller's own posts to verify ownership.
/// `Db` has no get-post-by-id, so the check re-uses the read tool's scoped
/// `list_posts(Some(seller_id), …)` with a generous window (500 rows, ~10x
/// the list tool's cap): any realistic seller history fits, and a target
/// outside the window is refused exactly like a foreign or nonexistent post
/// — no oracle either way.
const EDIT_POST_OWNERSHIP_SCAN_LIMIT: i64 = 500;

/// Identity read tools (owner personal account, L10 map): default page sizes
/// and the hard clamp, deliberately separate from the bot-side list caps
/// above. The personal account is the scarcer resource — sequential reads
/// ≤30/min are enforced inside the identity actor; these bounds only keep
/// any single MCP call small.
pub const DEFAULT_IDENTITY_LIMIT: i64 = 50;
pub const DEFAULT_SEARCH_LIMIT: i64 = 30;
pub const MAX_IDENTITY_LIMIT: i64 = 100;
/// `search_messages` query length bound, in characters.
pub const MAX_SEARCH_QUERY_CHARS: usize = 200;
/// `publish_as_user` text bound, in characters (Telegram's message limit).
pub const MAX_PUBLISH_AS_USER_CHARS: usize = 4096;

/// The identity tool set: reads through the operator's personal Telegram
/// account (user-identity design, decision 6). Owner-principal ONLY — a
/// principal carrying a seller id is refused in `mcp::dispatch` before
/// validation, and every handler repeats the check. Later phases extend this
/// set (`list_dm`) or gate their identity tools at Write/Sensitive tiers
/// (`reply_dm`, `publish_as_user`); nothing in this set may ever execute for
/// a per-seller token.
pub const IDENTITY_TOOLS: &[&str] = &[
    "read_channel",
    "read_thread",
    "search_messages",
    "unread_digest",
];

/// Whether `name` belongs to the owner-principal-only identity set.
pub fn is_identity_tool(name: &str) -> bool {
    IDENTITY_TOOLS.contains(&name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Read,
    Write,
    Sensitive,
}

/// Who is calling. v1 mints exactly one principal (the static owner bearer
/// token), but every handler threads it so per-seller tokens (v2) are
/// additive: handlers scope their queries by `seller_id`, where `None` means
/// the unscoped owner view.
#[derive(Debug, Clone)]
pub struct Principal {
    pub role: roles::Role,
    pub seller_id: Option<i64>,
}

impl Principal {
    pub const OWNER: Principal = Principal {
        role: roles::Role::Owner,
        seller_id: None,
    };
}

/// Lowercase role name for audit rows.
pub fn role_name(role: roles::Role) -> &'static str {
    match role {
        roles::Role::User => "user",
        roles::Role::Seller => "seller",
        roles::Role::Admin => "admin",
        roles::Role::Owner => "owner",
    }
}

pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
/// Handlers take owned clones (`WebState`/`Principal` are cheap-Clone) so the
/// boxed futures stay `'static`; an Arc'd closure (not a fn pointer) lets the
/// registry carry the generic `h()` wrapper without lifetime plumbing.
pub type Handler = Arc<dyn Fn(WebState, Principal, Value) -> HandlerFuture + Send + Sync>;
pub type Validator = fn(&Value) -> Result<(), String>;

pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub tier: Tier,
    pub input_schema: Value,
    pub annotations: Value,
    /// Pure argument check, run by the dispatcher BEFORE any handler runs so
    /// bad arguments fail with -32602 and zero side effects — including for
    /// sensitive tools, which must not park an approval for garbage args.
    pub validate: Validator,
    pub handler: Handler,
}

/// The static tool table. Built once via `OnceLock` (the schemas are runtime
/// JSON values, so a plain `static` cannot hold them).
pub fn registry() -> &'static [ToolSpec] {
    static REG: OnceLock<Vec<ToolSpec>> = OnceLock::new();
    REG.get_or_init(|| {
        vec![
            // ---- Tier: Read ----
            ToolSpec {
                name: "list_channels",
                description: "List the default target channel and each seller's channel.",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(list_channels),
            },
            ToolSpec {
                name: "list_plans",
                description: "List subscription plans.",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(list_plans),
            },
            ToolSpec {
                name: "list_posts",
                description: "List recent posts (stored records, newest first).",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT,
                                  "description": format!("How many posts to return (default {DEFAULT_LIST_LIMIT}).")}
                    }
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_limit,
                handler: h(list_posts),
            },
            ToolSpec {
                name: "subs_report",
                description: "Subscription report: counts by status. User ids are masked \
                              unless include_user_ids=true (the response is then flagged \
                              contains_pii).",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "include_user_ids": {"type": "boolean", "default": false,
                                             "description": "Include subscriber user ids (PII)."}
                    }
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_subs_report,
                handler: h(subs_report),
            },
            ToolSpec {
                name: "list_codes",
                description: "List recent activation codes, MASKED (first 4 chars). Full \
                              codes are only ever shown to the owner in the bot after an \
                              approved gen_codes.",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT,
                                  "description": format!("How many codes to return (default {DEFAULT_LIST_LIMIT}).")}
                    }
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_limit,
                handler: h(list_codes),
            },
            ToolSpec {
                name: "list_promos",
                description: "List promo codes for the seller (codes are not secret here).",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(list_promos),
            },
            ToolSpec {
                name: "list_referrals",
                description: "List referral records (masked).",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(list_referrals),
            },
            ToolSpec {
                name: "referral_stats",
                description: "Referral program stats (counts and earnings).",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(referral_stats),
            },
            ToolSpec {
                name: "analytics_summary",
                description: "Business analytics summary (revenue total, invoice counts, AOV, \
                               top products, repeat buyers, abandoned cart rate, customer \
                               count). Aggregated; no PII.",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(analytics_summary),
            },
             ToolSpec {
                 name: "list_customers",
                 description: "List CRM customers. User ids are masked unless \
                               include_user_ids=true (response flagged contains_pii).",
                 tier: Tier::Read,
                 input_schema: json!({
                     "type": "object",
                     "properties": {
                         "tag": {"type": "string"},
                         "status": {"type": "string"},
                         "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LIST_LIMIT,
                                   "description": format!("How many customers to return (default {DEFAULT_LIST_LIMIT}).")},
                         "include_user_ids": {"type": "boolean", "default": false,
                                              "description": "Include customer user ids (PII)."}
                     }
                 }),
                 annotations: json!({"readOnlyHint": true}),
                 validate: validate_list_customers,
                 handler: h(list_customers),
             },
            // ---- Identity reads (Tier: Read, owner-principal ONLY) ----
            // The per-seller refusal lives in mcp::dispatch (pre-validation)
            // and is repeated inside every handler; these specs only declare
            // the surface. tools/list still advertises the names — capability
            // discovery is fine; calling is what is gated.
            ToolSpec {
                name: "read_channel",
                description: "Read recent messages from a channel as the owner's personal \
                              Telegram account (read-only; owner credential only).",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {"type": "string",
                                    "description": "Channel to read (@name or numeric id as a string)."},
                        "before": {"type": "integer", "minimum": 1,
                                   "description": "Return messages strictly older than this message id (pagination)."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_IDENTITY_LIMIT,
                                  "description": format!("How many messages to return (default {DEFAULT_IDENTITY_LIMIT}).")}
                    },
                    "required": ["channel"]
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_read_channel,
                handler: h(read_channel),
            },
            ToolSpec {
                name: "read_thread",
                description: "Read the replies of one discussion thread as the owner's \
                              personal Telegram account (read-only; owner credential only).",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {"type": "string",
                                    "description": "Channel holding the thread (@name or numeric id as a string)."},
                        "msg_id": {"type": "integer", "minimum": 1,
                                   "description": "Root message id of the thread."},
                        "before": {"type": "integer", "minimum": 1,
                                   "description": "Return replies strictly older than this message id (pagination)."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_IDENTITY_LIMIT,
                                  "description": format!("How many replies to return (default {DEFAULT_IDENTITY_LIMIT}).")}
                    },
                    "required": ["channel", "msg_id"]
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_read_thread,
                handler: h(read_thread),
            },
            ToolSpec {
                name: "search_messages",
                description: "Search messages as the owner's personal Telegram account \
                              (read-only; owner credential only). `channel` is required and \
                              must be a channel already configured in the console.",
                tier: Tier::Read,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "minLength": 1, "maxLength": MAX_SEARCH_QUERY_CHARS,
                                  "description": "Search text (1-200 characters)."},
                        "channel": {"type": "string",
                                    "description": "Scope (@name or numeric id); must be a console-configured channel."},
                        "limit": {"type": "integer", "minimum": 1, "maximum": MAX_IDENTITY_LIMIT,
                                  "description": format!("How many matches to return (default {DEFAULT_SEARCH_LIMIT}).")}
                    },
                    "required": ["query", "channel"]
                }),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_search_messages,
                handler: h(search_messages),
            },
            ToolSpec {
                name: "unread_digest",
                description: "List chats with unread activity for the owner's personal \
                              Telegram account, newest first (read-only; owner credential only).",
                tier: Tier::Read,
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: json!({"readOnlyHint": true}),
                validate: validate_empty,
                handler: h(unread_digest),
            },
             // ---- Tier: Write ----
            ToolSpec {
                name: "publish_post",
                description: "Publish a text post to a channel (default channel unless \
                              `channel` is given). Executes immediately. Optional parse_mode: \
                              'html' enables Telegram HTML formatting (b/i/code/pre/a).",
                tier: Tier::Write,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {"type": "string",
                                    "description": "Target channel (@name or id); defaults to the configured default channel."},
                        "text": {"type": "string", "minLength": 1},
                        "parse_mode": {"type": "string",
                                       "description": "Optional parse_mode: 'html' enables Telegram HTML formatting (b/i/code/pre/a)."},
                        "bot_key": {"type": "string",
                                    "description": "Owner-only; seller credentials ignore it and act through their own bot."},
                        "bot_id": {"type": "integer",
                                   "description": "Owner-only; seller credentials ignore it and act through their own bot."}
                    },
                    "required": ["text"]
                }),
                annotations: json!({"readOnlyHint": false}),
                validate: validate_publish_post,
                handler: h(publish_post),
            },
            ToolSpec {
                name: "edit_post",
                description: "Edit the stored content of a logged post. Edits the stored \
                              record only; the already-sent Telegram message is not rewritten. \
                              Optional parse_mode: 'html' enables Telegram HTML formatting \
                              (b/i/code/pre/a).",
                tier: Tier::Write,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "post_id": {"type": "integer"},
                        "text": {"type": "string", "minLength": 1},
                        "parse_mode": {"type": "string",
                                       "description": "Optional parse_mode: 'html' enables Telegram HTML formatting (b/i/code/pre/a)."}
                    },
                    "required": ["post_id", "text"]
                }),
                annotations: json!({"readOnlyHint": false}),
                validate: validate_edit_post,
                handler: h(edit_post),
            },
             ToolSpec {
                 name: "customer_note",
                 description: "Add or update a note on a CRM customer (audited).",
                 tier: Tier::Write,
                 input_schema: json!({
                     "type": "object",
                     "properties": {
                         "seller_id": {"type": "integer",
                                       "description": "Seller scope (owner only; ignored for seller tokens)."},
                         "user_id": {"type": "integer"},
                         "note": {"type": "string", "minLength": 1}
                     },
                     "required": ["user_id", "note"]
                 }),
                 annotations: json!({"readOnlyHint": false}),
                 validate: validate_customer_note,
                 handler: h(customer_note),
             },
             // ---- Tier: Sensitive (approval-gated; the call path never executes) ----
            ToolSpec {
                name: "gen_codes",
                description: "Generate activation codes. Requires owner approval; the full \
                              codes are delivered to the owner's Telegram chat and are never \
                              returned over MCP.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "plan_id": {"type": "integer",
                                    "description": "Optional plan the codes unlock."},
                        "count": {"type": "integer", "minimum": 1, "maximum": MAX_CODES},
                        "days": {"type": "integer", "minimum": 1,
                                 "description": "Validity in days (when the code is not plan-bound)."},
                        "max_uses": {"type": "integer", "minimum": 1, "default": 1},
                        "length": {"type": "integer", "minimum": 1, "maximum": 32, "default": 12}
                    },
                    "required": ["count"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_gen_codes,
                handler: park_fn("gen_codes"),
            },
            ToolSpec {
                name: "create_promo",
                description: "Create a promo code. Requires owner approval.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "code": {"type": "string", "minLength": 1},
                        "promo_type": {"type": "string",
                                       "description": "percent or fixed"},
                        "value": {"type": "number", "exclusiveMinimum": 0},
                        "max_uses": {"type": "integer", "minimum": 1},
                        "min_purchase": {"type": "number", "minimum": 0},
                        "days": {"type": "integer", "minimum": 1,
                                 "description": "Validity in days (omit for no expiry)."}
                    },
                    "required": ["code", "promo_type", "value"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_create_promo,
                handler: park_fn("create_promo"),
            },
            ToolSpec {
                name: "delete_post",
                description: "Delete a stored post record. The stored record is deleted; the \
                              already-sent Telegram message is NOT unsent. Requires owner approval.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "post_id": {"type": "integer"}
                    },
                    "required": ["post_id"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_delete_post,
                handler: park_fn("delete_post"),
            },
            ToolSpec {
                name: "broadcast",
                description: "Send a text message to all known users. High blast radius; \
                              requires owner approval.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "text": {"type": "string", "minLength": 1},
                        "bot_key": {"type": "string",
                                    "description": "Owner-only; seller credentials ignore it and act through their own bot."},
                        "bot_id": {"type": "integer",
                                   "description": "Owner-only; seller credentials ignore it and act through their own bot."}
                    },
                    "required": ["text"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_broadcast,
                handler: park_fn("broadcast"),
            },
            ToolSpec {
                name: "ai_publish_post",
                description: "Publish an AI-generated post draft to its channel. Requires \
                              owner approval.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "draft_id": {"type": "integer", "minimum": 1},
                        "bot_key": {"type": "string",
                                    "description": "Owner-only; seller credentials ignore it and act through their own bot."},
                        "bot_id": {"type": "integer",
                                   "description": "Owner-only; seller credentials ignore it and act through their own bot."}
                    },
                    "required": ["draft_id"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_ai_publish_post,
                handler: park_fn("ai_publish_post"),
            },
            ToolSpec {
                name: "export_buyer_data",
                description: "Export all stored data for one buyer as JSON. Sensitive: requires \
                              owner approval; the export is delivered in the approval reply.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "seller_id": {"type": "integer"},
                        "user_id": {"type": "integer"}
                    },
                    "required": ["seller_id", "user_id"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_export_buyer_data,
                handler: park_fn("export_buyer_data"),
            },
            ToolSpec {
                name: "delete_buyer_data",
                description: "Delete all stored data for one buyer. Sensitive: requires owner \
                              approval. Irreversible.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "seller_id": {"type": "integer"},
                        "user_id": {"type": "integer"}
                    },
                    "required": ["seller_id", "user_id"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_delete_buyer_data,
                handler: park_fn("delete_buyer_data"),
            },
            ToolSpec {
                name: "publish_as_user",
                description: "Publish a text post to a channel AS the owner's personal Telegram \
                              account (user-identity design, Tier::Sensitive). The channel must \
                              be a console-configured channel bound to the personal identity \
                              ('Post as user…'). Requires owner approval; at execution the \
                              posting gate and server caps (≤5/hour, ≤15/day) apply.",
                tier: Tier::Sensitive,
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "channel": {"type": "string",
                                    "description": "Target channel; must be a console-configured channel bound to the personal identity (sender_identity = 'user')."},
                        "text": {"type": "string", "minLength": 1, "maxLength": MAX_PUBLISH_AS_USER_CHARS,
                                 "description": format!("Post text (1-{MAX_PUBLISH_AS_USER_CHARS} characters).")}
                    },
                    "required": ["channel", "text"]
                }),
                annotations: json!({"readOnlyHint": false, "destructiveHint": true,
                                    "x-approval-required": true}),
                validate: validate_publish_as_user,
                handler: park_fn("publish_as_user"),
            },
        ]
    })
}

pub fn find_tool(name: &str) -> Option<&'static ToolSpec> {
    registry().iter().find(|t| t.name == name)
}

/// Box an async handler fn into the shared `Handler` shape.
fn h<F, Fut>(f: F) -> Handler
where
    F: Fn(WebState, Principal, Value) -> Fut + Copy + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    Arc::new(move |state, principal, args| Box::pin(f(state, principal, args)))
}

// ---- Argument validators (pure; run before any handler) ----

/// Tools that take no arguments accept only an absent or empty object.
fn validate_empty(args: &Value) -> Result<(), String> {
    match args {
        Value::Null | Value::Object(_) => Ok(()),
        _ => Err("arguments must be an object".into()),
    }
}

fn get_int<'a>(args: &'a Value, key: &str) -> Result<Option<&'a Value>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_i64() => Ok(Some(v)),
        Some(_) => Err(format!("'{key}' must be an integer")),
    }
}

fn validate_limit(args: &Value) -> Result<(), String> {
    if let Some(v) = get_int(args, "limit")?
        && let Some(n) = v.as_i64()
        && !(1..=MAX_LIST_LIMIT).contains(&n)
    {
        return Err(format!("'limit' must be between 1 and {MAX_LIST_LIMIT}"));
    }
    Ok(())
}

fn validate_subs_report(args: &Value) -> Result<(), String> {
    match args.get("include_user_ids") {
        None | Some(Value::Null) => Ok(()),
        Some(v) if v.is_boolean() => Ok(()),
        Some(_) => Err("'include_user_ids' must be a boolean".into()),
    }
}

fn validate_text_field(args: &Value, key: &str) -> Result<(), String> {
    match args.get(key) {
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(()),
        Some(Value::String(_)) => Err(format!("'{key}' must not be empty")),
        Some(_) => Err(format!("'{key}' must be a string")),
        None => Err(format!("missing required argument '{key}'")),
    }
}

/// Map the optional `parse_mode` argument of the post tools to a teloxide
/// parse mode. Absent (or null) keeps the historical plain-text send — zero
/// drift for scripted callers; `"html"` passes the caller's text as-is so
/// the caller crafts the entities (`<b>`, `<i>`, `<code>`, `<pre>`,
/// `<a href>`). Any other value is rejected with an error naming the one
/// valid value. In html mode no escaping is added (the caller owns the
/// entities); Telegram itself rejects malformed HTML and that error flows
/// back to the caller as the tool error.
fn resolve_parse_mode(args: &Value) -> Result<Option<teloxide::types::ParseMode>, String> {
    match args.get("parse_mode") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s == "html" => Ok(Some(teloxide::types::ParseMode::Html)),
        Some(_) => Err(
            "'parse_mode' must be \"html\" (Telegram HTML formatting) or omitted for plain text"
                .into(),
        ),
    }
}

fn validate_publish_post(args: &Value) -> Result<(), String> {
    validate_text_field(args, "text")?;
    match args.get("channel") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if !s.trim().is_empty() => {}
        Some(_) => return Err("'channel' must be a non-empty string".into()),
    }
    resolve_parse_mode(args).map(|_| ())
}

fn validate_ai_publish_post(args: &Value) -> Result<(), String> {
    if get_int(args, "draft_id")?.is_none() {
        return Err("missing required integer argument 'draft_id'".into());
    }
    let id = args.get("draft_id").and_then(|v| v.as_i64()).unwrap_or(0);
    if id < 1 {
        return Err("'draft_id' must be >= 1".into());
    }
    Ok(())
}

fn validate_edit_post(args: &Value) -> Result<(), String> {
    if get_int(args, "post_id")?.is_none() {
        return Err("missing required integer argument 'post_id'".into());
    }
    validate_text_field(args, "text")?;
    resolve_parse_mode(args).map(|_| ())
}

fn validate_gen_codes(args: &Value) -> Result<(), String> {
    let Some(v) = get_int(args, "count")? else {
        return Err("missing required integer argument 'count'".into());
    };
    let count = v.as_i64().unwrap_or(0);
    if !(1..=MAX_CODES).contains(&count) {
        return Err(format!("'count' must be between 1 and {MAX_CODES}"));
    }
    if let Some(v) = get_int(args, "days")?
        && v.as_i64().unwrap_or(0) < 1
    {
        return Err("'days' must be >= 1".into());
    }
    if let Some(v) = get_int(args, "max_uses")?
        && v.as_i64().unwrap_or(0) < 1
    {
        return Err("'max_uses' must be >= 1".into());
    }
    if let Some(v) = get_int(args, "length")? {
        let l = v.as_i64().unwrap_or(0);
        if !(1..=32).contains(&l) {
            return Err("'length' must be between 1 and 32".into());
        }
    }
    Ok(())
}

fn validate_create_promo(args: &Value) -> Result<(), String> {
    match args.get("code") {
        Some(Value::String(s)) if !s.trim().is_empty() => {}
        Some(Value::String(_)) => return Err("'code' must not be empty".into()),
        Some(_) => return Err("'code' must be a string".into()),
        None => return Err("missing required argument 'code'".into()),
    }
    match args.get("promo_type") {
        Some(Value::String(s)) if s == "percent" || s == "fixed" => {}
        Some(Value::String(_)) => return Err("'promo_type' must be 'percent' or 'fixed'".into()),
        Some(_) => return Err("'promo_type' must be a string".into()),
        None => return Err("missing required argument 'promo_type'".into()),
    }
    match args.get("value") {
        Some(v) if v.is_number() => {
            let n = v.as_f64().unwrap_or(0.0);
            if n <= 0.0 {
                return Err("'value' must be > 0".into());
            }
        }
        Some(_) => return Err("'value' must be a number".into()),
        None => return Err("missing required argument 'value'".into()),
    }
    if let Some(v) = get_int(args, "max_uses")? {
        let n = v.as_i64().unwrap_or(0);
        if n < 1 {
            return Err("'max_uses' must be >= 1".into());
        }
    }
    match args.get("min_purchase") {
        None | Some(Value::Null) => {}
        Some(v) if v.is_number() => {
            let n = v.as_f64().unwrap_or(0.0);
            if n < 0.0 {
                return Err("'min_purchase' must be >= 0".into());
            }
        }
        Some(_) => return Err("'min_purchase' must be a number".into()),
    }
    if let Some(v) = get_int(args, "days")? {
        let n = v.as_i64().unwrap_or(0);
        if n < 1 {
            return Err("'days' must be >= 1".into());
        }
    }
    Ok(())
}

fn validate_delete_post(args: &Value) -> Result<(), String> {
    if get_int(args, "post_id")?.is_none() {
        return Err("missing required integer argument 'post_id'".into());
    }
    Ok(())
}

fn validate_broadcast(args: &Value) -> Result<(), String> {
    validate_text_field(args, "text")
}

fn validate_list_customers(args: &Value) -> Result<(), String> {
    if let Some(v) = args.get("tag")
        && !v.is_string()
    {
        return Err("'tag' must be a string".into());
    }
    if let Some(v) = args.get("status")
        && !v.is_string()
    {
        return Err("'status' must be a string".into());
    }
    validate_limit(args)?;
    match args.get("include_user_ids") {
        None | Some(Value::Null) => Ok(()),
        Some(v) if v.is_boolean() => Ok(()),
        Some(_) => Err("'include_user_ids' must be a boolean".into()),
    }
}

fn validate_customer_note(args: &Value) -> Result<(), String> {
    if get_int(args, "user_id")?.is_none() {
        return Err("missing required integer argument 'user_id'".into());
    }
    if let Some(v) = get_int(args, "seller_id")? {
        let n = v.as_i64().unwrap_or(0);
        if n < 0 {
            return Err("'seller_id' must be >= 0".into());
        }
    }
    validate_text_field(args, "note")
}

fn validate_export_buyer_data(args: &Value) -> Result<(), String> {
    if get_int(args, "seller_id")?.is_none() {
        return Err("missing required integer argument 'seller_id'".into());
    }
    if get_int(args, "user_id")?.is_none() {
        return Err("missing required integer argument 'user_id'".into());
    }
    Ok(())
}

fn validate_delete_buyer_data(args: &Value) -> Result<(), String> {
    if get_int(args, "seller_id")?.is_none() {
        return Err("missing required integer argument 'seller_id'".into());
    }
    if get_int(args, "user_id")?.is_none() {
        return Err("missing required integer argument 'user_id'".into());
    }
    Ok(())
}

/// Shared tail of the identity-read validators: optional `before` cursor
/// (a message id, so >= 1) and optional `limit` (1..=MAX_IDENTITY_LIMIT).
fn validate_identity_paging(args: &Value) -> Result<(), String> {
    if let Some(v) = get_int(args, "before")?
        && v.as_i64().unwrap_or(0) < 1
    {
        return Err("'before' must be >= 1".into());
    }
    if let Some(v) = get_int(args, "limit")?
        && let Some(n) = v.as_i64()
        && !(1..=MAX_IDENTITY_LIMIT).contains(&n)
    {
        return Err(format!(
            "'limit' must be between 1 and {MAX_IDENTITY_LIMIT}"
        ));
    }
    Ok(())
}

fn validate_read_channel(args: &Value) -> Result<(), String> {
    validate_text_field(args, "channel")?;
    validate_identity_paging(args)
}

/// `publish_as_user`: channel required and non-empty; text required,
/// 1..=4096 characters (Telegram's limit). The DB-bound checks — channel
/// configured AND bound to the personal identity, writes enabled — run in
/// the executor at approval time, so a park never carries a stale judgment.
fn validate_publish_as_user(args: &Value) -> Result<(), String> {
    validate_text_field(args, "channel")?;
    match args.get("text") {
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Err("'text' must not be empty".into());
            }
            let len = s.chars().count();
            if len > MAX_PUBLISH_AS_USER_CHARS {
                return Err(format!(
                    "'text' must be at most {MAX_PUBLISH_AS_USER_CHARS} characters"
                ));
            }
        }
        Some(_) => return Err("'text' must be a string".into()),
        None => return Err("missing required argument 'text'".into()),
    }
    Ok(())
}

fn validate_read_thread(args: &Value) -> Result<(), String> {
    validate_text_field(args, "channel")?;
    if get_int(args, "msg_id")?.is_none() {
        return Err("missing required integer argument 'msg_id'".into());
    }
    if args.get("msg_id").and_then(|v| v.as_i64()).unwrap_or(0) < 1 {
        return Err("'msg_id' must be >= 1".into());
    }
    validate_identity_paging(args)
}

fn validate_search_messages(args: &Value) -> Result<(), String> {
    match args.get("channel") {
        Some(Value::String(s)) => {
            if s.trim().is_empty() {
                return Err("'channel' must not be empty".into());
            }
        }
        Some(_) => return Err("'channel' must be a string".into()),
        None => return Err("missing required argument 'channel'".into()),
    }
    match args.get("query") {
        Some(Value::String(s)) => {
            let len = s.trim().chars().count();
            if len == 0 {
                return Err("'query' must not be empty".into());
            }
            if len > MAX_SEARCH_QUERY_CHARS {
                return Err(format!(
                    "'query' must be at most {MAX_SEARCH_QUERY_CHARS} characters"
                ));
            }
        }
        Some(_) => return Err("'query' must be a string".into()),
        None => return Err("missing required argument 'query'".into()),
    }
    match args.get("channel") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) if !s.trim().is_empty() => {}
        Some(_) => return Err("'channel' must be a non-empty string".into()),
    }
    validate_identity_paging(args)
}

// ---- Helpers shared by handlers ----

/// Mask an activation code for every read path: keep the first 4 characters,
/// never the rest. Codes are 12 ASCII chars from `util::gen_token`, so
/// char-based truncation is exact.
fn mask_code(code: &str) -> String {
    let head: String = code.chars().take(4).collect();
    format!("{head}…")
}

fn list_limit(args: &Value) -> i64 {
    args.get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT)
}

/// Park a sensitive call: nothing executes here, the args are stored verbatim
/// so the owner's approval replays exactly what the caller asked for.
async fn park(
    state: WebState,
    _principal: Principal,
    args: Value,
    tool: &'static str,
) -> Result<Value, String> {
    let args_json = serde_json::to_string(&args).unwrap_or_else(|_| "{}".into());
    let expires_at =
        (chrono::Utc::now() + chrono::Duration::seconds(APPROVAL_TTL_SECS)).to_rfc3339();
    let id = state.db.mcp_create_approval(tool, &args_json, &expires_at);
    Ok(json!({
        "requires_approval": true,
        "approval_id": id,
        "expires_at": expires_at,
        "how": "approve via /approve <id> in the bot (owner only)"
    }))
}

/// Build the park handler for a sensitive tool name.
fn park_fn(tool: &'static str) -> Handler {
    h(move |state, principal, args| park(state, principal, args, tool))
}

// ---- Tier Read handlers ----

async fn list_channels(
    state: WebState,
    principal: Principal,
    _args: Value,
) -> Result<Value, String> {
    let default = state.db.get_config("channel");
    let sellers: Vec<Value> = if let Some(sid) = principal.seller_id {
        vec![json!({"seller_id": sid, "channel": state.db.seller_channel(sid)})]
    } else {
        state
            .db
            .list_sellers()
            .into_iter()
            .map(|sid| json!({"seller_id": sid, "channel": state.db.seller_channel(sid)}))
            .collect()
    };
    Ok(json!({"default": default, "sellers": sellers}))
}

async fn list_plans(state: WebState, principal: Principal, _args: Value) -> Result<Value, String> {
    // PlanRow: (name, price, asset, duration_days, channel_id, active, seller_id)
    let items: Vec<Value> = state
        .db
        .list_plans(principal.seller_id)
        .into_iter()
        .map(|p| {
            json!({
                "name": p.0, "price": p.1, "asset": p.2, "duration_days": p.3,
                "channel_id": p.4, "active": p.5, "seller_id": p.6,
            })
        })
        .collect();
    Ok(json!({"items": items, "total": items.len()}))
}

async fn list_posts(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    // PostRow: (id, seller_id, chat_id, message_id, kind, content,
    //           created_at, sender) — sender appended in Phase 2
    // ('bot' | 'user', posts sender audit).
    let items: Vec<Value> = state
        .db
        .list_posts(principal.seller_id, list_limit(&args))
        .into_iter()
        .map(|p| {
            json!({
                "id": p.0, "seller_id": p.1, "chat_id": p.2, "message_id": p.3,
                "kind": p.4, "content": p.5, "created_at": p.6, "sender": p.7,
            })
        })
        .collect();
    Ok(json!({"items": items, "total": items.len()}))
}

async fn subs_report(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    let include_ids = args
        .get("include_user_ids")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let scope = principal.seller_id;
    let by_status: Value = state
        .db
        .subscription_status_counts(scope)
        .into_iter()
        .map(|(status, count)| (status, Value::from(count)))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    if !include_ids {
        // Default: aggregates only — no subscriber identities leave the box.
        return Ok(json!({"by_status": by_status, "contains_pii": false}));
    }
    // list_subscriptions row: (id, seller_id, user_id, channel_id, status, expires_at)
    let rows: Vec<Value> = state
        .db
        .list_subscriptions(scope, 100)
        .into_iter()
        .map(|s| {
            json!({
                "id": s.0, "seller_id": s.1, "user_id": s.2, "channel_id": s.3,
                "status": s.4, "expires_at": s.5,
            })
        })
        .collect();
    Ok(json!({
        "by_status": by_status,
        "rows": rows,
        "contains_pii": true
    }))
}

async fn list_customers(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let tag = args.get("tag").and_then(|v| v.as_str());
    let status = args.get("status").and_then(|v| v.as_str());
    let limit = list_limit(&args);
    let include_ids = args
        .get("include_user_ids")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let scopes: Vec<i64> = if let Some(sid) = principal.seller_id {
        vec![sid]
    } else {
        let mut v = state.db.list_sellers();
        v.push(0);
        v
    };
    let mut items: Vec<Value> = Vec::new();
    for sid in scopes {
        for (uid, last_seen, tags, spent, orders, st) in state.db.list_customers(sid, tag, status) {
            if items.len() >= limit as usize {
                break;
            }
            let notes = state
                .db
                .get_customer(sid, uid)
                .and_then(|c| c.5)
                .unwrap_or_default();
            if include_ids {
                items.push(json!({
                    "seller_id": sid,
                    "user_id": uid,
                    "last_seen": last_seen,
                    "tags": tags,
                    "total_spent": spent,
                    "order_count": orders,
                    "status": st,
                    "notes": notes,
                }));
            } else {
                let masked = format!("{}…", uid.to_string().chars().take(2).collect::<String>());
                items.push(json!({
                    "seller_id": sid,
                    "user_id": masked,
                    "last_seen": last_seen,
                    "tags": tags,
                    "total_spent": spent,
                    "order_count": orders,
                    "status": st,
                    "notes": notes,
                }));
            }
        }
    }
    Ok(json!({"items": items, "total": items.len(), "contains_pii": include_ids}))
}

async fn customer_note(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let user_id = args.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let note = args
        .get("note")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let seller_id = if let Some(sid) = principal.seller_id {
        sid
    } else {
        args.get("seller_id").and_then(|v| v.as_i64()).unwrap_or(0)
    };
    state.db.upsert_customer(seller_id, user_id);
    let ok = state.db.update_customer_notes(seller_id, user_id, &note);
    Ok(json!({"ok": ok, "seller_id": seller_id, "user_id": user_id}))
}

async fn list_codes(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    let items: Vec<Value> = state
        .db
        .list_codes(principal.seller_id, list_limit(&args))
        .into_iter()
        .map(|c| {
            json!({
                "code": mask_code(&c.0), "plan_id": c.1, "value_days": c.2,
                "uses": c.3, "max_uses": c.4, "seller_id": c.5, "revoked": c.6,
            })
        })
        .collect();
    Ok(json!({"items": items, "total": items.len()}))
}

async fn list_promos(state: WebState, principal: Principal, _args: Value) -> Result<Value, String> {
    let rows = if let Some(sid) = principal.seller_id {
        state.db.list_promo_codes(sid)
    } else {
        state.db.list_all_promo_codes()
    };
    let items: Vec<Value> = rows
        .into_iter()
        .map(|p| {
            let active = p.11 != 0;
            json!({
                "code": p.0,
                "seller_id": p.1,
                "type": p.2,
                "promo_type": p.2,
                "value": p.3,
                "currency": p.4,
                "max_uses": p.5,
                "uses": p.6,
                "min_purchase": p.7,
                "scope": p.8,
                "scope_id": p.9,
                "expiry": p.10,
                "expires_at": p.10,
                "active": active,
                "active_int": p.11,
                "created_at": p.12,
            })
        })
        .collect();
    Ok(json!({"items": items, "total": items.len()}))
}

async fn list_referrals(
    state: WebState,
    principal: Principal,
    _args: Value,
) -> Result<Value, String> {
    let mut items: Vec<Value> = Vec::new();
    if let Some(sid) = principal.seller_id {
        for r in state.db.list_referrals(sid, None) {
            items.push(json!({
                "seller_id": sid,
                "id": r.0,
                "referrer_id": r.1,
                "referee_id": r.2,
                "code": mask_code(&r.3),
                "status": r.4,
                "reward_value": r.5,
            }));
        }
    } else {
        let mut scopes = state.db.list_sellers();
        scopes.push(0);
        for sid in scopes {
            for r in state.db.list_referrals(sid, None) {
                items.push(json!({
                    "seller_id": sid,
                    "id": r.0,
                    "referrer_id": r.1,
                    "referee_id": r.2,
                    "code": mask_code(&r.3),
                    "status": r.4,
                    "reward_value": r.5,
                }));
            }
        }
    }
    Ok(json!({"items": items, "total": items.len()}))
}

async fn referral_stats(
    state: WebState,
    principal: Principal,
    _args: Value,
) -> Result<Value, String> {
    let mut total = 0i64;
    let mut pending = 0i64;
    let mut rewarded = 0i64;
    let mut earnings = 0.0f64;
    let scopes: Vec<i64> = if let Some(sid) = principal.seller_id {
        vec![sid]
    } else {
        let mut v = state.db.list_sellers();
        v.push(0);
        v
    };
    for sid in scopes {
        for r in state.db.list_referrals(sid, None) {
            total += 1;
            if r.4 == "rewarded" {
                rewarded += 1;
                earnings += r.5;
            } else {
                pending += 1;
            }
        }
    }
    Ok(json!({
        "total": total,
        "pending": pending,
        "rewarded": rewarded,
        "earnings": earnings,
    }))
}

async fn analytics_summary(
    state: WebState,
    principal: Principal,
    _args: Value,
) -> Result<Value, String> {
    let seller_ids: Vec<i64> = if let Some(sid) = principal.seller_id {
        vec![sid]
    } else {
        let mut ids = state.db.list_sellers();
        if ids.is_empty() {
            ids.push(0);
        }
        ids
    };

    let mut revenue_total = 0.0_f64;
    let mut invoice_total: i64 = 0;
    let mut invoice_paid: i64 = 0;
    let mut repeat_total: i64 = 0;
    let mut repeat_buyers: i64 = 0;
    let mut abandoned: i64 = 0;
    let mut recovered: i64 = 0;
    let mut customer_count: i64 = 0;
    let mut top_products: Vec<(i64, i64)> = Vec::new();

    for sid in &seller_ids {
        revenue_total += state.db.analytics_revenue_total(*sid);
        let (t, p) = state.db.analytics_invoice_counts(*sid);
        invoice_total += t;
        invoice_paid += p;
        top_products.extend(state.db.analytics_top_products(*sid, 10));
        let (tot, rep) = state.db.analytics_repeat_buyers(*sid);
        repeat_total += tot;
        repeat_buyers += rep;
        let (ab, rec) = state.db.analytics_abandoned_cart_rate(*sid);
        abandoned += ab;
        recovered += rec;
        customer_count += state.db.analytics_customer_count(*sid);
    }

    let aov = if invoice_paid > 0 {
        revenue_total / invoice_paid as f64
    } else {
        0.0
    };
    let abandoned_rate = if abandoned + recovered > 0 {
        (abandoned as f64 / (abandoned + recovered) as f64) * 100.0
    } else {
        0.0
    };

    let mut top_map: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    for (pid, cnt) in &top_products {
        *top_map.entry(*pid).or_insert(0) += *cnt;
    }
    let mut top_sorted: Vec<(i64, i64)> = top_map.into_iter().collect();
    top_sorted.sort_by_key(|x| std::cmp::Reverse(x.1));
    top_sorted.truncate(10);
    let top_items: Vec<Value> = top_sorted
        .iter()
        .map(|(pid, cnt)| json!({"product_id": pid, "count": cnt}))
        .collect();

    Ok(json!({
        "revenue_total": revenue_total,
        "invoice_counts": {"total": invoice_total, "paid": invoice_paid},
        "aov": aov,
        "top_products": top_items,
        "repeat_buyers": {"total": repeat_total, "repeat": repeat_buyers},
        "abandoned_cart": {"abandoned": abandoned, "recovered": recovered, "rate": abandoned_rate},
        "customer_count": customer_count,
        "masked": true
    }))
}

// ---- Identity read handlers (owner-principal only; see IDENTITY_TOOLS) ----

/// Defense in depth for the L10 owner-principal floor: `mcp::dispatch`
/// refuses principals carrying a seller id before validation even runs; this
/// repeats the check so the handler itself stays safe if a future call path
/// ever reaches it directly.
fn require_owner_principal(principal: &Principal) -> Result<(), String> {
    if principal.seller_id.is_some() {
        return Err("identity tools require the owner credential".into());
    }
    Ok(())
}

/// Page size for identity reads. The validators already reject out-of-range
/// values with -32602; this clamps defensively so the actor never sees more
/// than MAX_IDENTITY_LIMIT regardless.
fn identity_limit(args: &Value, default: i64) -> usize {
    args.get("limit")
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
        .clamp(1, MAX_IDENTITY_LIMIT) as usize
}

/// The honest tool-result strings the L10 contract fixes for identity reads:
/// retryable limits carry their retry seconds, the terminal auth failure says
/// re-pair, and state errors say where to fix it. Mapped errors become tool
/// errors (isError results), never panics or protocol-level faults. None of
/// the strings embed message content or phone numbers; the fallback Display
/// forms are constructed secret-free (see `identity::ops`).
fn identity_error_message(e: &crate::identity::IdentityError) -> String {
    use crate::identity::IdentityError as E;
    match e {
        E::RateLimited(secs) => format!("Rate limited; retry in {secs}s"),
        E::FloodWait(secs) => format!("Telegram flood control: retry in {secs}s"),
        E::AuthKeyDuplicated => "identity disabled — re-pair required".into(),
        E::NotConfigured => "identity not set up".into(),
        E::Locked => "identity locked — unlock in console".into(),
        other => other.to_string(),
    }
}

async fn read_channel(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    require_owner_principal(&principal)?;
    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let before = args.get("before").and_then(|v| v.as_i64());
    let limit = identity_limit(&args, DEFAULT_IDENTITY_LIMIT);
    match state.identity.history(&channel, before, limit).await {
        Ok(messages) => Ok(json!({
            "channel": channel,
            "items": messages,
            "total": messages.len()
        })),
        Err(e) => Err(identity_error_message(&e)),
    }
}

async fn read_thread(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    require_owner_principal(&principal)?;
    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let msg_id = args.get("msg_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let before = args.get("before").and_then(|v| v.as_i64());
    let limit = identity_limit(&args, DEFAULT_IDENTITY_LIMIT);
    match state
        .identity
        .thread_replies(&channel, msg_id, before, limit)
        .await
    {
        Ok(messages) => Ok(json!({
            "channel": channel,
            "msg_id": msg_id,
            "items": messages,
            "total": messages.len()
        })),
        Err(e) => Err(identity_error_message(&e)),
    }
}

async fn search_messages(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    require_owner_principal(&principal)?;
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    // Absent/empty channel means the account-wide (global) search.
    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let limit = identity_limit(&args, DEFAULT_SEARCH_LIMIT);
    match state
        .identity
        .search(&query, channel.as_deref(), limit)
        .await
    {
        Ok(messages) => Ok(json!({
            "channel": channel,
            "items": messages,
            "total": messages.len()
        })),
        Err(e) => Err(identity_error_message(&e)),
    }
}

async fn unread_digest(
    state: WebState,
    principal: Principal,
    _args: Value,
) -> Result<Value, String> {
    require_owner_principal(&principal)?;
    match state.identity.unread_digest().await {
        Ok(dialogs) => Ok(json!({"items": dialogs, "total": dialogs.len()})),
        Err(e) => Err(identity_error_message(&e)),
    }
}

// ---- Bot selection (multi-bot "web-bot-scoping", N3) ----

/// Resolve the Telegram bot a tool call acts through, from the optional
/// owner-only selectors advertised as `bot_key` / `bot_id` (`bot` is accepted
/// as a string alias of `bot_key`).
///
/// Owner principals: no selector → the primary bot. An explicit selector never
/// falls back to primary — an unknown / disabled / unconfigured `bot_id` or
/// key is an error, and selectors that name different bots are an error (two
/// forms naming the SAME bot are accepted).
///
/// Seller principals (every non-owner role): the selectors are IGNORED OUTRIGHT
/// — the anti-spoof floor. A per-seller credential always acts through the bot
/// mapped to its `seller_id` (primary only when the seller has no mapping), so
/// even a valid `bot_id`/`bot_key` can never reach another tenant's bot.
pub(super) fn resolve_tool_bot(
    state: &WebState,
    principal: &Principal,
    args: &Value,
) -> Result<Arc<crate::bots::BotContext>, String> {
    if principal.role != roles::Role::Owner {
        let sid = principal
            .seller_id
            .ok_or_else(|| "seller principal missing seller_id".to_string())?;
        return Ok(state.bots.for_seller(sid));
    }

    fn string_selector(args: &Value, field: &str) -> Result<Option<String>, String> {
        match args.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_str()
                .map(|s| Some(s.to_string()))
                .ok_or_else(|| format!("{field} must be a string")),
        }
    }

    let mut picks: Vec<Arc<crate::bots::BotContext>> = Vec::new();
    match args.get("bot_id") {
        None | Some(Value::Null) => {}
        Some(v) => {
            let id = v
                .as_i64()
                .ok_or_else(|| "bot_id must be an integer".to_string())?;
            picks.push(
                state
                    .bots
                    .get(id)
                    .ok_or_else(|| "unknown or disabled bot".to_string())?,
            );
        }
    }
    let mut keys: Vec<String> = Vec::new();
    if let Some(k) = string_selector(args, "bot_key")? {
        keys.push(k);
    }
    if let Some(k) = string_selector(args, "bot")? {
        keys.push(k);
    }
    for key in keys {
        picks.push(
            state
                .bots
                .for_bot_key(&key)
                .ok_or_else(|| "unknown or disabled bot".to_string())?,
        );
    }
    let Some(first) = picks.first() else {
        return Ok(state.bots.primary.clone());
    };
    if picks.iter().any(|p| p.id != first.id) {
        return Err("conflicting bot selectors".into());
    }
    Ok(first.clone())
}

// ---- Tier Write handlers ----

async fn publish_post(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    // Mirrors the web API's posts_create: arg channel > configured default.
    let ch = args
        .get("channel")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.db.get_config("channel").unwrap_or_default());
    if ch.is_empty() {
        return Err("no channel configured (set the default channel first)".into());
    }
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    // Absent parse_mode keeps the historical plain-text send; "html" sends
    // the caller's text as-is under Telegram's HTML mode. The stored record
    // keeps the raw text either way.
    let parse_mode = resolve_parse_mode(&args)?;
    // Unlike the web API (which logs a 0 message id), a bot failure is
    // reported as a tool error so an AI caller knows nothing was posted.
    // Multi-bot: the send goes through the principal's effective bot (owner
    // selector or the seller's mapped bot), never blindly through primary.
    let bot_ctx = resolve_tool_bot(&state, &principal, &args)?;
    let seller_id = principal.seller_id.unwrap_or(0);
    // Outbound content gate BEFORE transport. This is a NON-ContentSender
    // path (raw Bot::send_message), so it must be checked here; seller scope
    // comes from the principal, never a global 0.
    crate::handlers::moderation::gate_outbound(&state.db, seller_id, Some(bot_ctx.id), &text)
        .map_err(|hold| hold.to_string())?;
    let mut send = bot_ctx
        .bot
        .send_message(crate::util::to_chat_id(&ch), text.clone());
    if let Some(pm) = parse_mode {
        send = send.parse_mode(pm);
    }
    match send.await {
        Ok(sent) => {
            crate::metrics::bot_sent();
            let _ = state
                .db
                .log_post(seller_id, &ch, sent.id.0, "text", Some(&text), "bot");
            Ok(json!({"ok": true, "message_id": sent.id.0, "channel": ch}))
        }
        Err(e) => Err(format!("send failed: {e}")),
    }
}

async fn edit_post(state: WebState, principal: Principal, args: Value) -> Result<Value, String> {
    let post_id = args
        .get("post_id")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    // Arg-shape parity with publish_post: parse_mode is accepted and
    // validated the same way, but this tool edits the STORED RECORD ONLY
    // (no edit_message_text — the sent Telegram message is never rewritten
    // by design), so there is no send to decorate and the raw text is
    // stored verbatim either way.
    let _parse_mode = resolve_parse_mode(&args)?;
    // Tenancy: the update itself is by-id and unscoped, so a seller principal
    // must first prove the target is one of their own posts. The check re-uses
    // the read tool's exact scoped query (`list_posts(Some(seller_id), …)`,
    // bounded by EDIT_POST_OWNERSHIP_SCAN_LIMIT); owner principals (no seller
    // id) keep the historical unscoped behavior. A foreign post and a
    // nonexistent one answer with the SAME failure — the message never
    // distinguishes existence.
    if let Some(seller_id) = principal.seller_id {
        let owns = state
            .db
            .list_posts(Some(seller_id), EDIT_POST_OWNERSHIP_SCAN_LIMIT)
            .into_iter()
            .any(|p| p.0 == post_id);
        if !owns {
            return Err("post not found or not yours".into());
        }
    }
    match state.db.update_post_content(post_id, text) {
        Ok(()) => Ok(json!({"ok": true, "post_id": post_id,
                            "note": "stored record updated; the sent Telegram message was not rewritten"})),
        Err(e) => Err(format!("update failed: {e}")),
    }
}

// ---- Tier Sensitive executors (approval path only — see execute_sensitive) ----

async fn exec_gen_codes(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let count = args.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
    let plan_id = args.get("plan_id").and_then(|v| v.as_i64());
    let days = args
        .get("days")
        .and_then(|v| v.as_i64())
        .map(|d| d.clamp(1, crate::constants::MAX_PLAN_DAYS));
    let max_uses = args.get("max_uses").and_then(|v| v.as_i64()).unwrap_or(1);
    let length = args
        .get("length")
        .and_then(|v| v.as_i64())
        .unwrap_or(12)
        .clamp(1, 32) as usize;
    let seller_id = principal.seller_id.unwrap_or(0);
    let codes: Vec<String> = (0..count)
        .map(|_| {
            state
                .db
                .create_code(seller_id, plan_id, days, max_uses, None, length)
        })
        .collect();
    Ok(json!({
        "codes": codes,
        "count": codes.len(),
        "note": "codes are secrets: deliver them only to the intended buyers"
    }))
}

async fn exec_delete_post(
    state: WebState,
    _principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let post_id = args
        .get("post_id")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();
    state
        .db
        .delete_post(post_id)
        .map_err(|e| format!("delete failed: {e}"))?;
    Ok(json!({"ok": true, "post_id": post_id,
              "note": "stored record deleted; the sent Telegram message was not unsent"}))
}

async fn exec_broadcast(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let users = match principal.seller_id {
        Some(sid) => state.db.list_users_for_seller(sid),
        None => state.db.list_users(),
    };
    let total = users.len();
    // Send loop shared with the /broadcast command (crate::broadcast_to_users):
    // undeliverable users are skipped, the rest still get the message.
    // Multi-bot: sends through the principal's effective bot (owner selector
    // or the seller's mapped bot).
    let bot_ctx = resolve_tool_bot(&state, &principal, &args)?;
    // Outbound content gate BEFORE the broadcast loop. `broadcast_to_users`
    // is a raw-Bot helper (non-ContentSender), so the one gate here covers the
    // whole fan-out. Seller scope comes from the principal.
    let seller_id = principal.seller_id.unwrap_or(0);
    crate::handlers::moderation::gate_outbound(&state.db, seller_id, Some(bot_ctx.id), text)
        .map_err(|hold| hold.to_string())?;
    let sent = crate::broadcast_to_users(&bot_ctx.bot, &users, text).await;
    Ok(json!({"sent": sent, "total": total}))
}

async fn exec_ai_publish_post(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let draft_id = args.get("draft_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let draft = state
        .db
        .get_ai_post_draft(draft_id)
        .ok_or_else(|| "draft not found".to_string())?;
    let (_id, seller_id, channel, _template, content, _status, _c, _u, _m) = draft;
    // Multi-bot: the publish goes through the principal's effective bot (owner
    // selector or the seller's mapped bot), never blindly through primary.
    let bot_ctx = resolve_tool_bot(&state, &principal, &args)?;
    // Outbound content gate BEFORE transport. The draft's own seller id is
    // the authoritative scope (owner drafts carry 0, i.e. the global floor);
    // fall back to the principal's seller for parity with publish_post.
    let gate_seller = if seller_id == 0 {
        principal.seller_id.unwrap_or(0)
    } else {
        seller_id
    };
    crate::handlers::moderation::gate_outbound(&state.db, gate_seller, Some(bot_ctx.id), &content)
        .map_err(|hold| hold.to_string())?;
    match bot_ctx
        .bot
        .send_message(crate::util::to_chat_id(&channel), content.clone())
        .await
    {
        Ok(sent) => {
            crate::metrics::bot_sent();
            let _ = state.db.log_post(
                seller_id,
                &channel,
                sent.id.0,
                "text",
                Some(&content),
                "bot",
            );
            let _ = state
                .db
                .mark_ai_post_draft_published(draft_id, sent.id.0 as i64);
            Ok(json!({
                "ok": true,
                "draft_id": draft_id,
                "message_id": sent.id.0,
                "channel": channel
            }))
        }
        Err(e) => Err(format!("send failed: {e}")),
    }
}

async fn exec_export_buyer_data(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let seller_id = args.get("seller_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let user_id = args.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let sid = principal.seller_id.unwrap_or(seller_id);
    let data = state.db.export_buyer_data(sid, user_id);
    let parsed: Value = serde_json::from_str(&data).unwrap_or(Value::String(data));
    Ok(json!({"seller_id": sid, "user_id": user_id, "data": parsed}))
}

async fn exec_delete_buyer_data(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let seller_id = args.get("seller_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let user_id = args.get("user_id").and_then(|v| v.as_i64()).unwrap_or(0);
    let sid = principal.seller_id.unwrap_or(seller_id);
    let n = state.db.delete_buyer_data(sid, user_id);
    Ok(json!({"ok": true, "seller_id": sid, "user_id": user_id, "deleted_rows": n}))
}

async fn exec_create_promo(
    state: WebState,
    principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let code = args
        .get("code")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let promo_type = args
        .get("promo_type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let value = args.get("value").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let max_uses = args.get("max_uses").and_then(|v| v.as_i64());
    let min_purchase = args
        .get("min_purchase")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let days = args.get("days").and_then(|v| v.as_i64());
    let expires_at = days.map(|d| (chrono::Utc::now() + chrono::Duration::days(d)).to_rfc3339());
    let seller_id = principal.seller_id.unwrap_or(0);
    state
        .db
        .create_promo_code(
            seller_id,
            &code,
            &promo_type,
            value,
            None,
            max_uses,
            min_purchase,
            "all",
            None,
            expires_at.as_deref(),
        )
        .map_err(|e| format!("create failed: {e}"))?;
    Ok(json!({"ok": true, "code": code, "seller_id": seller_id}))
}

/// Execute an approved `publish_as_user`: post `text` to `channel` as the
/// operator's personal Telegram account. Runs ONLY from the owner's
/// `/approve` (Sensitive tier — the MCP call path parks, never executes).
///
/// The DB-bound checks run here, at execution time, so a parked approval is
/// judged against the world as it is when approved, not when it was parked:
/// the channel must still be console-configured AND bound to the personal
/// identity (`sender_identity = 'user'` — one check covers both, since the
/// value requires the row), and the L10 T2 "Enable posting" gate must be
/// confirmed. Sends go through the `Sender::User` resolution; the identity
/// actor enforces the server caps (≤5/hour, ≤15/day) and the flood gates.
async fn exec_publish_as_user(
    state: WebState,
    _principal: Principal,
    args: Value,
) -> Result<Value, String> {
    let channel = args
        .get("channel")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if state.db.get_channel_sender_identity(&channel).as_deref() != Some("user") {
        return Err(format!(
            "channel '{channel}' is not bound to the personal identity; bind it first in the console (Channels → 'Post as user…')"
        ));
    }
    if !crate::identity::user_writes_enabled(&state.db) {
        return Err(
            "posting as the personal account is disabled; enable posting in the console first"
                .into(),
        );
    }
    let sender = state.bots.for_channel_sender(&channel);
    let crate::identity::Sender::User(user) = sender else {
        // The binding flipped between the check and the resolution (or the
        // resolution disagreed): fail closed — never post as the bot from a
        // publish_as_user approval.
        return Err("channel is no longer bound to the personal identity".into());
    };
    let text_len = text.chars().count();
    // L10 gate: this channel-handle form is gated centrally inside
    // `UserContext::send_text_to` (identity/mod.rs) before the actor is
    // consulted — owner identity scope 0 / no bot, i.e. global floor only.
    // No second gate here, so the decision is audited exactly once.
    let outcome = user.send_text_to(&channel, &text).await;
    // Audit detail for the execution attempt: the channel and the text
    // LENGTH only — the text itself never lands in the audit trail.
    state.db.mcp_audit(
        "publish_as_user",
        "owner",
        None,
        outcome.is_ok(),
        Some(&format!("channel={channel} text_len={text_len}")),
    );
    match outcome {
        Ok(sent) => {
            // Sender audit (user-identity design §1): this post was sent by
            // the personal identity, not the bot.
            let seller_id = state.db.get_channel_seller_id(&channel).unwrap_or(0);
            let _ = state.db.log_post(
                seller_id,
                &channel,
                i32::try_from(sent.message_id).unwrap_or(0),
                "text",
                Some(&text),
                "user",
            );
            Ok(json!({
                "ok": true,
                "chat_id": sent.chat_id,
                "message_id": sent.message_id,
                "channel": channel,
                "sender": "user"
            }))
        }
        // SendError Display forms are honest and content-free by contract.
        Err(e) => Err(e.to_string()),
    }
}

/// Execute an approved sensitive tool. This is the ONLY path that runs the
/// sensitive executors, and it re-runs the tool's own validator first so the
/// approval replay can never drift from what the call path would have done.
pub async fn execute_sensitive(
    state: WebState,
    principal: &Principal,
    tool: &str,
    args: Value,
) -> Result<Value, String> {
    let spec = find_tool(tool).ok_or_else(|| format!("unknown tool '{tool}'"))?;
    if spec.tier != Tier::Sensitive {
        return Err(format!("tool '{tool}' is not approval-gated"));
    }
    (spec.validate)(&args)?;
    match tool {
        "gen_codes" => exec_gen_codes(state, principal.clone(), args).await,
        "create_promo" => exec_create_promo(state, principal.clone(), args).await,
        "delete_post" => exec_delete_post(state, principal.clone(), args).await,
        "broadcast" => exec_broadcast(state, principal.clone(), args).await,
        "ai_publish_post" => exec_ai_publish_post(state, principal.clone(), args).await,
        "export_buyer_data" => exec_export_buyer_data(state, principal.clone(), args).await,
        "delete_buyer_data" => exec_delete_buyer_data(state, principal.clone(), args).await,
        "publish_as_user" => exec_publish_as_user(state, principal.clone(), args).await,
        other => Err(format!("no executor for tool '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_code_keeps_only_the_head() {
        assert_eq!(mask_code("ABCD1234EFGH"), "ABCD…");
        // Short/odd inputs must not panic and must not reveal more than asked.
        assert_eq!(mask_code("AB"), "AB…");
    }

    #[test]
    fn referral_tools_registered_as_read() {
        let lr = find_tool("list_referrals").expect("list_referrals registered");
        assert_eq!(lr.tier, Tier::Read);
        let rs = find_tool("referral_stats").expect("referral_stats registered");
        assert_eq!(rs.tier, Tier::Read);
    }

    #[test]
    fn registry_tiers_and_annotations() {
        for t in registry() {
            match t.tier {
                Tier::Read => assert_eq!(
                    t.annotations["readOnlyHint"], true,
                    "{} must be readOnly",
                    t.name
                ),
                Tier::Write => assert_eq!(t.annotations["readOnlyHint"], false),
                Tier::Sensitive => {
                    assert_eq!(t.annotations["destructiveHint"], true, "{}", t.name);
                    assert_eq!(t.annotations["x-approval-required"], true, "{}", t.name);
                }
            }
        }
    }

    /// The gen_codes schema must not carry any string default: a secret-bearing
    /// default would embed a real code shape in the advertised tool metadata.
    #[test]
    fn gen_codes_schema_has_no_string_defaults() {
        let spec = find_tool("gen_codes").unwrap();
        for (k, v) in spec.input_schema["properties"].as_object().unwrap() {
            assert!(
                v.get("default").map(|d| !d.is_string()).unwrap_or(true),
                "property {k} has a string default"
            );
        }
    }

    #[test]
    fn validators_reject_bad_shapes() {
        assert!(validate_gen_codes(&json!({"count": 0})).is_err());
        assert!(validate_gen_codes(&json!({"count": 51})).is_err());
        assert!(validate_gen_codes(&json!({"count": "3"})).is_err());
        assert!(validate_gen_codes(&json!({"count": 5, "days": 0})).is_err());
        assert!(validate_gen_codes(&json!({"count": 5})).is_ok());
        assert!(validate_publish_post(&json!({"text": "  "})).is_err());
        assert!(validate_publish_post(&json!({"text": "hi"})).is_ok());
        assert!(validate_broadcast(&json!({})).is_err());
        assert!(validate_delete_post(&json!({})).is_err());
        assert!(validate_delete_post(&json!({"post_id": 3})).is_ok());
        assert!(validate_limit(&json!({"limit": 51})).is_err());
        assert!(validate_limit(&json!({})).is_ok());
    }

    // ---- Identity reads: registration, validation, error mapping ----

    #[test]
    fn identity_tools_registered_as_owner_only_reads() {
        for name in IDENTITY_TOOLS {
            let spec = find_tool(name).unwrap_or_else(|| panic!("{name} must be registered"));
            assert_eq!(spec.tier, Tier::Read, "{name}");
            assert_eq!(spec.annotations["readOnlyHint"], true, "{name}");
        }
        // The registry carries exactly the gated set — a renamed or added
        // identity tool must be reflected in IDENTITY_TOOLS or the owner
        // gate in mcp::dispatch would miss it.
        let gated_in_registry = registry()
            .iter()
            .filter(|t| IDENTITY_TOOLS.contains(&t.name))
            .count();
        assert_eq!(gated_in_registry, IDENTITY_TOOLS.len());
    }

    #[test]
    fn identity_validators_match_the_schemas() {
        // read_channel: channel required; paging bounded.
        assert!(validate_read_channel(&json!({})).is_err());
        assert!(validate_read_channel(&json!({"channel": " "})).is_err());
        assert!(validate_read_channel(&json!({"channel": "@chan", "limit": 0})).is_err());
        assert!(validate_read_channel(&json!({"channel": "@chan", "limit": 101})).is_err());
        assert!(validate_read_channel(&json!({"channel": "@chan", "limit": "5"})).is_err());
        assert!(validate_read_channel(&json!({"channel": "@chan", "before": 0})).is_err());
        assert!(validate_read_channel(&json!({"channel": "@chan"})).is_ok());
        assert!(
            validate_read_channel(&json!({"channel": "@chan", "before": 10, "limit": 100})).is_ok()
        );
        // read_thread: msg_id required and >= 1.
        assert!(validate_read_thread(&json!({"channel": "@chan"})).is_err());
        assert!(validate_read_thread(&json!({"channel": "@chan", "msg_id": 0})).is_err());
        assert!(validate_read_thread(&json!({"channel": "@chan", "msg_id": "7"})).is_err());
        assert!(validate_read_thread(&json!({"channel": "@chan", "msg_id": 7})).is_ok());
        // search_messages: query 1..=200 chars; channel required and non-empty
        // (F3 scoping — global search was removed from the identity surface).
        assert!(validate_search_messages(&json!({})).is_err());
        assert!(validate_search_messages(&json!({"query": "hi"})).is_err());
        assert!(validate_search_messages(&json!({"query": "   "})).is_err());
        assert!(validate_search_messages(&json!({"query": "x".repeat(201)})).is_err());
        assert!(
            validate_search_messages(&json!({"query": "x".repeat(200), "channel": "@chan"}))
                .is_ok()
        );
        assert!(validate_search_messages(&json!({"query": "hi", "channel": ""})).is_err());
        assert!(validate_search_messages(&json!({"query": "hi", "channel": "@chan"})).is_ok());
        // unread_digest takes no arguments beyond an (empty) object.
        assert!(validate_empty(&json!({})).is_ok());
    }

    #[test]
    fn identity_error_mapping_is_honest_and_content_free() {
        use crate::identity::IdentityError as E;
        assert_eq!(
            identity_error_message(&E::RateLimited(42)),
            "Rate limited; retry in 42s"
        );
        assert_eq!(
            identity_error_message(&E::FloodWait(30)),
            "Telegram flood control: retry in 30s"
        );
        assert_eq!(
            identity_error_message(&E::AuthKeyDuplicated),
            "identity disabled — re-pair required"
        );
        assert_eq!(
            identity_error_message(&E::NotConfigured),
            "identity not set up"
        );
        assert_eq!(
            identity_error_message(&E::Locked),
            "identity locked — unlock in console"
        );
        // The unconnected handle's error stays honest: it names the state,
        // not a vague failure.
        assert_eq!(
            identity_error_message(&E::NotConnected),
            "personal identity subsystem is not running"
        );
    }

    #[test]
    fn require_owner_principal_refuses_seller_tokens() {
        assert!(require_owner_principal(&Principal::OWNER).is_ok());
        let seller = Principal {
            role: roles::Role::Seller,
            seller_id: Some(7),
        };
        assert_eq!(
            require_owner_principal(&seller).unwrap_err(),
            "identity tools require the owner credential"
        );
    }

    // ---- publish_as_user (Tier: Sensitive) ----

    #[test]
    fn publish_as_user_registered_as_sensitive() {
        let spec = find_tool("publish_as_user").expect("publish_as_user must be registered");
        assert_eq!(spec.tier, Tier::Sensitive);
        assert_eq!(spec.annotations["readOnlyHint"], false);
        assert_eq!(spec.annotations["destructiveHint"], true);
        assert_eq!(spec.annotations["x-approval-required"], true);
        // Both args are required by the advertised schema.
        let required = spec.input_schema["required"].as_array().unwrap();
        assert!(required.contains(&json!("channel")));
        assert!(required.contains(&json!("text")));
    }

    #[test]
    fn publish_as_user_validator_bounds_channel_and_text() {
        assert!(validate_publish_as_user(&json!({})).is_err());
        assert!(validate_publish_as_user(&json!({"channel": "@c"})).is_err());
        assert!(validate_publish_as_user(&json!({"text": "hi"})).is_err());
        assert!(validate_publish_as_user(&json!({"channel": " ", "text": "hi"})).is_err());
        assert!(validate_publish_as_user(&json!({"channel": "@c", "text": ""})).is_err());
        assert!(validate_publish_as_user(&json!({"channel": "@c", "text": "   "})).is_err());
        assert!(validate_publish_as_user(&json!({"channel": "@c", "text": 5})).is_err());
        assert!(
            validate_publish_as_user(&json!({"channel": "@c", "text": "x".repeat(4097)})).is_err()
        );
        assert!(
            validate_publish_as_user(&json!({"channel": "@c", "text": "x".repeat(4096)})).is_ok()
        );
        assert!(validate_publish_as_user(&json!({"channel": "@c", "text": "hi"})).is_ok());
    }

    // ---- parse_mode (publish_post / edit_post) ----

    #[test]
    fn resolve_parse_mode_maps_absent_null_and_html_only() {
        use teloxide::types::ParseMode;
        // Absent or null: the historical plain-text path, zero drift.
        assert_eq!(resolve_parse_mode(&json!({"text": "hi"})), Ok(None));
        assert_eq!(
            resolve_parse_mode(&json!({"text": "hi", "parse_mode": null})),
            Ok(None)
        );
        // The one accepted value maps to teloxide's HTML mode.
        assert_eq!(
            resolve_parse_mode(&json!({"text": "hi", "parse_mode": "html"})),
            Ok(Some(ParseMode::Html))
        );
        // Anything else is rejected with an error naming the valid value.
        for bad in ["markdown", "HTML", "MarkdownV2", ""] {
            let err = resolve_parse_mode(&json!({"parse_mode": bad})).unwrap_err();
            assert!(err.contains("\"html\""), "{bad}: {err}");
        }
        assert!(resolve_parse_mode(&json!({"parse_mode": 5})).is_err());
    }

    #[test]
    fn post_tool_validators_enforce_parse_mode_values() {
        // Absent stays accepted on both tools (existing behavior untouched).
        assert!(validate_publish_post(&json!({"text": "hi"})).is_ok());
        assert!(validate_edit_post(&json!({"post_id": 1, "text": "hi"})).is_ok());
        // "html" accepted on both tools.
        assert!(validate_publish_post(&json!({"text": "hi", "parse_mode": "html"})).is_ok());
        assert!(
            validate_edit_post(&json!({"post_id": 1, "text": "hi", "parse_mode": "html"})).is_ok()
        );
        // Anything else is rejected with the same valid-value error on both.
        for bad in [json!("markdown"), json!("HTML"), json!(5)] {
            let pub_args = json!({"text": "hi", "parse_mode": bad});
            let edit_args = json!({"post_id": 1, "text": "hi", "parse_mode": bad});
            let err = validate_publish_post(&pub_args).unwrap_err();
            assert!(err.contains("\"html\""), "{bad}: {err}");
            assert_eq!(err, resolve_parse_mode(&pub_args).unwrap_err());
            assert_eq!(err, validate_edit_post(&edit_args).unwrap_err());
        }
    }

    /// Minimal WebState whose primary bot points its Bot API at `api_url`
    /// (wiremock) — the direct-handler mirror of the mcp tests' mcp_state +
    /// with_bot_manager seam, for exercising the real teloxide send path.
    fn send_test_state(api_url: &str) -> (WebState, tempfile::TempDir) {
        use crate::config::Config;
        use crate::db::Db;
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db").to_str().unwrap().to_string();
        let db = Db::new(&db_path).unwrap();
        let cfg = Config {
            owner_id: 1,
            db_path,
            crypto_token: None,
            ai_enabled: false,
            ai_provider: "openai".to_string(),
            ai_api_style: "openai".to_string(),
            ai_base_url: "https://api.openai.com/v1".to_string(),
            ai_api_key: None,
            ai_model: "gpt-4o-mini".to_string(),
            ai_daily_token_budget: None,
            mcp_owner_token: None,
            telegram_api_id: None,
            telegram_api_hash: None,
            bots: Vec::new(),
            bots_legacy: true,
        };
        let db = Arc::new(db);
        let shared = Arc::new(cfg.clone());
        let bot = teloxide::Bot::new("0:placeholder")
            .set_api_url(api_url.parse::<reqwest::Url>().unwrap());
        let primary = Arc::new(crate::bots::BotContext {
            id: 0,
            key: "primary".to_string(),
            bot,
            owner_id: 1,
            shared,
            db: db.clone(),
        });
        let bots = Arc::new(crate::bots::BotManager {
            bots: vec![primary.clone()],
            primary,
            db: db.clone(),
            identity: crate::identity::IdentityHandle::unconnected(),
        });
        let state = WebState {
            db: db.clone(),
            cfg,
            bot: teloxide::Bot::new("0:setup-placeholder"),
            bots,
            identity: crate::identity::IdentityHandle::unconnected(),
            login_attempts: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
            base_path: String::new(),
        };
        (state, dir)
    }

    /// End to end over the real teloxide send path (wiremock Bot API):
    /// parse_mode "html" serializes Telegram's `"parse_mode": "HTML"` on the
    /// wire, the absent-arg send carries NO parse_mode key at all (zero
    /// drift), and the stored record keeps the raw text either way.
    #[tokio::test]
    async fn publish_post_parse_mode_reaches_the_send_call() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let reply = json!({
            "ok": true,
            "result": {
                "message_id": 42,
                "date": 1_700_000_000,
                "chat": {"id": 123, "type": "private", "first_name": "T"},
                "text": "hi"
            }
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply))
            .mount(&server)
            .await;

        let (state, _dir) = send_test_state(&server.uri());

        let out = publish_post(
            state.clone(),
            Principal::OWNER,
            json!({"channel": "123", "text": "<b>hi</b>", "parse_mode": "html"}),
        )
        .await
        .expect("html send must succeed");
        assert_eq!(out["message_id"], 42);
        let out = publish_post(
            state.clone(),
            Principal::OWNER,
            json!({"channel": "123", "text": "hi"}),
        )
        .await
        .expect("plain send must succeed");
        assert_eq!(out["message_id"], 42);

        let hits = server.received_requests().await.unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits[0].url.path().ends_with("/SendMessage"),
            "{:?}",
            hits[0].url
        );
        // html mode reaches the wire as Telegram's HTML parse mode.
        let first: Value = serde_json::from_slice(&hits[0].body).unwrap();
        assert_eq!(first["parse_mode"], "HTML", "{first}");
        assert_eq!(first["text"], "<b>hi</b>");
        // Absent parse_mode: exactly the historical body, no parse_mode key.
        let second: Value = serde_json::from_slice(&hits[1].body).unwrap();
        assert!(second.get("parse_mode").is_none(), "{second}");
        assert_eq!(second["text"], "hi");
        // The stored record keeps the raw text either way (kind "text").
        let posts = state.db.list_posts(None, 10);
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[1].4, "text");
        assert_eq!(posts[1].5.as_deref(), Some("<b>hi</b>"));
        assert_eq!(posts[0].4, "text");
        assert_eq!(posts[0].5.as_deref(), Some("hi"));
    }

    /// Regression: `publish_post` is a NON-`ContentSender` seller-content path
    /// (it calls `Bot::send_message` directly). A seller-scoped floor term must
    /// hold it BEFORE transport — the mock server sees zero requests and the
    /// error carries the opaque `moderation_hold` marker (never the body).
    #[tokio::test]
    async fn publish_post_seller_term_holds_before_transport() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let reply = json!({
            "ok": true,
            "result": {
                "message_id": 7,
                "date": 1_700_000_000,
                "chat": {"id": 123, "type": "private", "first_name": "T"},
                "text": "x"
            }
        });
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reply))
            .mount(&server)
            .await;

        let (state, _dir) = send_test_state(&server.uri());
        let seller_id = 77i64;
        assert!(
            state
                .db
                .add_blocked_word(seller_id, "forbidden", "hide", true)
        );
        let principal = Principal {
            role: crate::roles::Role::Seller,
            seller_id: Some(seller_id),
        };
        let err = publish_post(
            state.clone(),
            principal,
            json!({"channel": "123", "text": "a forbidden draft"}),
        )
        .await
        .expect_err("seller term must hold the post");
        assert!(err.contains("moderation_hold"), "got {err:?}");
        assert!(!err.contains("forbidden"), "no body in the error");
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "no send may reach Telegram while held"
        );
        // The other seller is unaffected by seller 77's term.
        let other = Principal {
            role: crate::roles::Role::Seller,
            seller_id: Some(78),
        };
        publish_post(
            state,
            other,
            json!({"channel": "123", "text": "a forbidden draft"}),
        )
        .await
        .expect("other seller passes");
    }
}
