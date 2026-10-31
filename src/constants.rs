//! Shared constants used across the bot, web UI, and HTTP API.

/// Maximum subscription length (in days) any flow may create — 10 years.
/// Single source of truth so bot and web limits stay identical.
pub const MAX_PLAN_DAYS: i64 = 3650;

// BOOKING_HOLD_MINUTES lives in `crate::db` (kept there); re-exported here
// for callers that prefer the constants module.
pub use crate::db::BOOKING_HOLD_MINUTES;
