//! Personal-account (MTProto) identity layer.
//!
//! Owner-owned Telegram-user identity alongside the bot identities (ratified
//! user-identity design; AUTO_ROUTE Phase 1). Layout:
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
//! This file also carries the L11 sender contract: [`ContentSender`],
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

/// A successfully posted message, exactly as the L11 contract defines it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentMessage {
    pub chat_id: i64,
    pub message_id: i64,
}

/// Whether posting as the personal account is enabled (the L10 T2 "Enable
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
    /// the process restarts (L10 flood-halt policy).
    #[error(
        "personal identity sends are halted after repeated Telegram flood control; an owner surface reset (or restart) is required"
    )]
    SurfaceHalted,
    /// The L10 T2 "Enable posting" gate has not been confirmed: the operator
    /// has not recorded `identity_writes_enabled` in the config (see
    /// [`user_writes_enabled`]). Deliberately distinct from
    /// [`SendError::NotAuthorized`] — that means "not signed in", this means
    /// "signed in but posting is switched off" — so downstream copy can point
    /// at the actual fix instead of a re-login.
    #[error("posting as your personal account is disabled; enable posting in the console first")]
    WritesDisabled,
    /// A server-enforced send cap was hit (L10: posts ≤5/hour, ≤15/day).
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
    /// The L10 T2 gate: refuse before any transport work while posting is
    /// disabled. Checked by every send form below.
    fn check_writes_enabled(&self) -> Result<(), SendError> {
        if user_writes_enabled(&self.db) {
            Ok(())
        } else {
            Err(SendError::WritesDisabled)
        }
    }

    /// Send to a channel handle (`@name` or a numeric id as a string). The
    /// [`ContentSender`] trait fixes the numeric chat-id form for the uniform
    /// posting surface; this inherent method carries the DB channel-handle
    /// form (the canonical chat identifiers of this feature are the
    /// configured channel strings — the same resolution the read path uses).
    /// Same gates, same actor caps.
    pub async fn send_text_to(&self, channel: &str, text: &str) -> Result<SentMessage, SendError> {
        self.check_writes_enabled()?;
        self.handle.send_text_to(channel, text).await
    }
}

impl ContentSender for UserContext {
    fn identity(&self) -> SenderIdentity {
        SenderIdentity::User
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<SentMessage, SendError> {
        self.check_writes_enabled()?;
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
