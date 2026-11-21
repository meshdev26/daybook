//! Personal-account (MTProto) identity layer.
//!
//! Owner-owned Telegram-user identity alongside the bot identities (ratified
//! user-identity design, Phase 1). Layout:
//!
//! - [`ops`] — the testability seam: [`MtprotoOps`] (the MTProto surface the
//!   actor drives, implemented for real by `GrammersOps` and by a scripted
//!   fake in tests), [`IdentityError`], [`LoginStep`] and the normalized
//!   message/dialog wire types.
//! - [`tl`] — the raw-TL wrapper (`messages.getReplies`, session snapshot
//!   codec); the only place version-pinned grammers calls may live.
//! - [`actor`] — the actor task, [`IdentityHandle`] and
//!   [`IdentityManager`]; one task owns the client, sequential reads with a
//!   ≤30/min cap, FloodWait handling and the encrypted-session lifecycle.
//! - [`session`] — the AES-256-GCM blob store (Phase 0a).
//!
//! This file also carries the architecture sender contract: [`ContentSender`],
//! [`Sender`], [`SentMessage`], [`SendError`], [`SenderIdentity`]. Posting
//! resolves through `BotManager::for_channel_sender` (see `src/bots.rs`);
//! payment/delivery paths never see `Sender`.

pub mod actor;
pub mod ops;
pub mod session;
pub mod tl;

use std::future::Future;
use std::sync::Arc;

pub use actor::{IdentityHandle, IdentityManager, IdentityState, IdentityStatus};
pub use ops::{IdentityError, LoginStep, NormalizedDialog, NormalizedMessage};
pub use session::{SessionStore, SessionStoreError};

/// A successfully posted message, exactly as the architecture contract defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentMessage {
    pub chat_id: i64,
    pub message_id: i64,
}

/// Whether posting as the personal account is enabled (the security owner-approval "Enable
/// posting" gate, design decision 5): the operator's explicit confirmation is
/// recorded as the `identity_writes_enabled` config row — any value counts;
/// the row's existence IS the confirmation. Every user send consults this at
/// the sender boundary ([`UserContext`]); the MCP `publish_as_user` executor
/// checks it too as defense in depth. Reads are unaffected.
///
/// The check deliberately lives with the CALLERS (not inside the actor): the
/// actor owns transport state, this gate owns operator consent, and the two
/// evolve independently. In-memory actor caps and this DB-backed gate are
/// separate mechanisms — both can refuse a send.
pub fn user_writes_enabled(db: &crate::db::Db) -> bool {
    db.get_config("identity_writes_enabled").is_some()
}

/// Post failures, discriminated for UX copy and retry policy.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("Telegram flood control: retry after {0}s")]
    FloodWait(u64),
    /// The identity cannot post (not signed in / subsystem not running).
    /// Deliberately loud: a user-bound channel never falls back to the bot.
    #[error("personal identity is not authorized to post")]
    NotAuthorized,
    #[error("chat not found")]
    ChatNotFound,
    /// The send surface is flood-halted (≥3 FloodWaits in an hour, or a
    /// single >30min wait): terminal until the owner resets the surfaces or
    /// the process restarts (security flood-halt policy).
    #[error(
        "personal identity sends are halted after repeated Telegram flood control; an owner surface reset (or restart) is required"
    )]
    SurfaceHalted,
    /// The security owner-approval "Enable posting" gate has not been confirmed: the operator
    /// has not recorded `identity_writes_enabled` in the config (see
    /// [`user_writes_enabled`]). Deliberately distinct from
    /// [`SendError::NotAuthorized`] — that means "not signed in", this means
    /// "signed in but posting is switched off" — so downstream copy can point
    /// at the actual fix instead of a re-login.
    #[error("posting as your personal account is disabled; enable posting in the console first")]
    WritesDisabled,
    /// A server-enforced send cap was hit (security: posts ≤5/hour, ≤15/day).
    /// `limit` names the cap for honest downstream copy;
    /// `retry_after_secs` is when the sliding window frees a slot.
    #[error("personal identity post cap reached ({limit}); retry after {retry_after_secs}s")]
    CapReached {
        limit: &'static str,
        retry_after_secs: u64,
    },
}

/// Which identity a sender posts as. `Bot` carries the bot's row id for
/// audit columns; the user identity is the single operator account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SenderIdentity {
    Bot(i64),
    User,
}

/// The uniform posting surface. Static dispatch only (no `dyn`, no
/// `async_trait`); the future is `Send` because senders are used from axum
/// handlers and spawned tasks.
pub trait ContentSender {
    fn identity(&self) -> SenderIdentity;
    fn send_text(
        &self,
        chat_id: i64,
        text: &str,
    ) -> impl Future<Output = Result<SentMessage, SendError>> + Send;
}

/// The operator's personal-account sender: a thin wrapper over the identity
/// actor handle. Exists so `Sender::User` has a concrete (cheaply clonable)
/// type that implements [`ContentSender`].
#[derive(Clone)]
pub struct UserContext {
    pub handle: IdentityHandle,
    /// The app database — source of the writes-enabled gate. The gate is
    /// enforced HERE at the sender boundary (not inside the actor) so every
    /// user send — web composer, MCP tool, anything resolving through
    /// `Sender::User` — fails closed identically while posting is disabled.
    pub db: Arc<crate::db::Db>,
}

impl UserContext {
    /// The security owner-approval gate: refuse before any transport work while posting is
    /// disabled. Checked by every send form below.
    fn check_writes_enabled(&self) -> Result<(), SendError> {
        if user_writes_enabled(&self.db) {
            Ok(())
        } else {
            Err(SendError::WritesDisabled)
        }
    }

    /// The security outbound content gate for the owner identity. Both send forms
    /// (`send_text` and `send_text_to`) route through this single adapter: the
    /// policy decision itself lives in the central
    /// [`crate::handlers::moderation::gate_outbound`] helper. The owner
    /// personal account has no bot/session seller, so it applies the GLOBAL
    /// FLOOR ONLY (`seller_id = 0`, `bot_id = None`) — no seller rule or
    /// per-bot override can be attributed to an owner-identity send. Fails
    /// closed on degraded enforcement.
    fn check_outbound(&self, text: &str) -> Result<(), SendError> {
        crate::handlers::moderation::gate_outbound(&self.db, 0, None, text)
            .map_err(|hold| SendError::Transport(hold.to_string()))
    }

    /// Send to a channel handle (`@name` or a numeric id as a string). The
    /// [`ContentSender`] trait fixes the numeric chat-id form for the uniform
    /// posting surface; this inherent method carries the DB channel-handle
    /// form (the canonical chat identifiers of this feature are the
    /// configured channel strings — the same resolution the read path uses).
    /// Same gates, same actor caps — including the outbound content gate,
    /// which is why MCP `publish_as_user` (the caller of this form) can no
    /// longer bypass the documented floor.
    pub async fn send_text_to(&self, channel: &str, text: &str) -> Result<SentMessage, SendError> {
        self.check_writes_enabled()?;
        self.check_outbound(text)?;
        self.handle.send_text_to(channel, text).await
    }
}

impl ContentSender for UserContext {
    fn identity(&self) -> SenderIdentity {
        SenderIdentity::User
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<SentMessage, SendError> {
        self.check_writes_enabled()?;
        // Same single owner-identity outbound gate as `send_text_to` (global
        // floor only, seller 0 / no bot). The writes gate still fires first,
        // so the security "Enable posting" behavior is unchanged.
        self.check_outbound(text)?;
        self.handle.send_text(chat_id, text).await
    }
}

/// Resolved sender for a channel: the bot that owns the channel, or the
/// operator's personal identity when the channel is explicitly bound
/// `'user'` (`channels.sender_identity`). Payment/delivery paths never
/// receive a `Sender` — they stay on `for_channel`/`for_seller` by design.
pub enum Sender {
    Bot(Arc<crate::bots::BotContext>),
    User(Arc<UserContext>),
}

impl Sender {
    pub async fn send_text(&self, chat_id: i64, text: &str) -> Result<SentMessage, SendError> {
        match self {
            Sender::Bot(b) => b.send_text(chat_id, text).await,
            Sender::User(u) => u.send_text(chat_id, text).await,
        }
    }

    pub fn identity(&self) -> SenderIdentity {
        match self {
            Sender::Bot(b) => b.identity(),
            Sender::User(u) => u.identity(),
        }
    }

    pub fn is_bot(&self) -> bool {
        matches!(self, Sender::Bot(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (tempfile::TempDir, Arc<crate::db::Db>) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(crate::db::Db::new(dir.path().join("t.db").to_str().unwrap()).unwrap());
        (dir, db)
    }

    /// The writes gate: with no `identity_writes_enabled` row the sender
    /// refuses BEFORE any transport state is consulted — and the error is the
    /// specific `WritesDisabled`, not the ambiguous `NotAuthorized`.
    #[tokio::test]
    async fn user_context_refuses_sends_while_writes_are_disabled() {
        let (_dir, db) = test_db();
        let ctx = UserContext {
            handle: IdentityHandle::unconnected(),
            db: db.clone(),
        };
        assert!(!user_writes_enabled(&db));
        let err = ctx.send_text(42, "hi").await.unwrap_err();
        assert!(matches!(err, SendError::WritesDisabled), "got {err:?}");
        let err = ctx.send_text_to("@chan", "hi").await.unwrap_err();
        assert!(matches!(err, SendError::WritesDisabled), "got {err:?}");
    }

    /// With the gate confirmed, the send proceeds to the actor path — the
    /// unconnected handle then fails loudly with `NotAuthorized` (never a
    /// bot fallback). The two error classes stay distinguishable.
    #[tokio::test]
    async fn user_context_passes_through_once_writes_are_enabled() {
        let (_dir, db) = test_db();
        db.set_config("identity_writes_enabled", "1");
        assert!(user_writes_enabled(&db));
        let ctx = UserContext {
            handle: IdentityHandle::unconnected(),
            db,
        };
        let err = ctx.send_text(42, "hi").await.unwrap_err();
        assert!(matches!(err, SendError::NotAuthorized), "got {err:?}");
        let err = ctx.send_text_to("@chan", "hi").await.unwrap_err();
        assert!(matches!(err, SendError::NotAuthorized), "got {err:?}");
    }

    /// Outbound moderation gate on the owner-identity `ContentSender`
    /// choke point. Uses seller 0 / no bot (global floor only, per the
    /// documented decision). With posting enabled, a matching seller-0 term
    /// holds before the actor is consulted; a clean text passes through to
    /// the actor (which fails loudly as unconnected, never a bot fallback).
    #[tokio::test]
    async fn user_context_send_text_gate_holds_then_passes() {
        let (_dir, db) = test_db();
        // Global namespace seller 0 stands in for the owner-floor-only scope.
        assert!(db.add_blocked_word(0, "forbidden", "hide", true));
        db.set_config("identity_writes_enabled", "1");
        let ctx = UserContext {
            handle: IdentityHandle::unconnected(),
            db: db.clone(),
        };

        let err = ctx.send_text(42, "a forbidden draft").await.unwrap_err();
        assert!(
            matches!(&err, SendError::Transport(m) if m.contains("moderation_hold")),
            "got {err:?}"
        );

        assert!(db.remove_blocked_word(0, "forbidden"));
        let err = ctx.send_text(42, "clean draft").await.unwrap_err();
        assert!(
            matches!(err, SendError::NotAuthorized),
            "clean text must reach the actor, got {err:?}"
        );

        // The security writes gate still takes precedence over the moderation gate.
        let (_dir2, db2) = test_db();
        assert!(db2.add_blocked_word(0, "forbidden", "hide", true));
        let no_writes = UserContext {
            handle: IdentityHandle::unconnected(),
            db: db2,
        };
        let err = no_writes.send_text(42, "forbidden").await.unwrap_err();
        assert!(matches!(err, SendError::WritesDisabled), "got {err:?}");
    }

    /// Regression: the CHANNEL-HANDLE send form (`send_text_to`) — the form
    /// MCP `publish_as_user` uses — is a NON-`ContentSender` path that must be
    /// held by a matching global-floor term BEFORE the actor is consulted.
    #[tokio::test]
    async fn user_context_send_text_to_holds_matching_floor_term() {
        let (_dir, db) = test_db();
        assert!(db.add_blocked_word(0, "forbidden", "hide", true));
        db.set_config("identity_writes_enabled", "1");
        let ctx = UserContext {
            handle: IdentityHandle::unconnected(),
            db: db.clone(),
        };
        let err = ctx
            .send_text_to("@chan", "a forbidden draft")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, SendError::Transport(m) if m.contains("moderation_hold")),
            "publish_as_user path must be held, got {err:?}"
        );
        assert!(db.remove_blocked_word(0, "forbidden"));
        let err = ctx.send_text_to("@chan", "clean draft").await.unwrap_err();
        assert!(
            matches!(err, SendError::NotAuthorized),
            "clean text must reach the actor, got {err:?}"
        );
    }

    /// `Sender::User` (the resolution `BotManager::for_channel_sender`
    /// returns for a user-bound channel) carries the same gate.
    #[tokio::test]
    async fn sender_user_arm_enforces_the_writes_gate() {
        let (_dir, db) = test_db();
        let sender = Sender::User(Arc::new(UserContext {
            handle: IdentityHandle::unconnected(),
            db: db.clone(),
        }));
        let err = sender.send_text(42, "hi").await.unwrap_err();
        assert!(matches!(err, SendError::WritesDisabled), "got {err:?}");
        db.set_config("identity_writes_enabled", "1");
        let err = sender.send_text(42, "hi").await.unwrap_err();
        assert!(matches!(err, SendError::NotAuthorized), "got {err:?}");
        assert_eq!(sender.identity(), SenderIdentity::User);
        assert!(!sender.is_bot());
    }
}
