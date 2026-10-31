//! Testability seam between the identity actor and MTProto (ratified
//! user-identity design, Phase 1 deliverable 1).
//!
//! grammers' [`grammers_client::Client`] opens real sockets and cannot be
//! unit-tested, so everything the actor needs from it is expressed as the
//! [`MtprotoOps`] trait. Two implementations exist:
//!
//! - [`super::actor::GrammersOps`] — the real one, over `grammers::Client`;
//! - `FakeOps` (actor.rs tests) — scripted responses + call recording.
//!
//! The trait uses native async-fn-in-trait via return-position `impl Trait`
//! with an explicit `+ Send` bound (no `async_trait` crate): the futures must
//! be `Send` because the actor task is spawned with `tokio::spawn`, and
//! `async fn` in traits does not auto-impose `Send` on its returned future.
//!
//! Login state between steps (the `LoginToken`/`PasswordToken`) lives inside
//! the implementation, mirroring grammers' own API where `request_login_code`
//! returns a token the caller then feeds back to `sign_in`. The actor only
//! observes [`LoginStep`] outcomes.

use serde::{Deserialize, Serialize};

/// Errors surfaced by the identity subsystem. Variants the UX layer maps to
/// user-facing copy are kept discriminated (wrong passphrase must never read
/// as "network down"); transport-level details are collapsed into
/// [`IdentityError::Transport`].
#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("personal identity is not configured (missing TELEGRAM_API_ID / TELEGRAM_API_HASH)")]
    NotConfigured,
    #[error("personal identity subsystem is not running")]
    NotConnected,
    #[error("personal identity is locked; unlock with the passphrase first")]
    Locked,
    #[error("personal identity is not signed in")]
    NotAuthorized,
    #[error("wrong unlock passphrase (or the session blob was tampered with)")]
    WrongPassphrase,
    #[error(
        "session blob was written by grammers {blob_grammers} but this build embeds \
         {crate_grammers}; re-pair the personal identity"
    )]
    VersionMismatch {
        blob_grammers: String,
        crate_grammers: String,
    },
    #[error("the login code was rejected by Telegram")]
    InvalidCode,
    #[error("the 2FA password was rejected by Telegram")]
    InvalidPassword,
    #[error("Telegram requires the 2FA password to complete sign-in")]
    PasswordRequired,
    #[error("sign-up with an official client is required; this app cannot register accounts")]
    SignUpRequired,
    #[error("read rate limit reached; retry in {0}s")]
    RateLimited(u64),
    #[error("chat is not an explicitly configured channel: {0}")]
    ChatNotAllowed(String),
    #[error(
        "personal identity {0} are halted after repeated Telegram flood control; \
         an owner surface reset (or restart) is required"
    )]
    SurfaceHalted(&'static str),
    #[error("message cursor is out of the supported range")]
    InvalidCursor,
    #[error("Telegram flood control: retry after {0}s")]
    FloodWait(u64),
    #[error(
        "the session authorization key is used by another client; the identity is disabled \
         until it is re-paired"
    )]
    AuthKeyDuplicated,
    #[error("peer not found: {0}")]
    PeerNotFound(String),
    #[error("operation not allowed while the identity is {0}")]
    InvalidState(&'static str),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("identity internal error: {0}")]
    Internal(String),
}

impl IdentityError {
    /// True for errors that mean the transport/pool is unhealthy and the
    /// actor should back off before the next request (reconnect gate).
    pub fn is_transport(&self) -> bool {
        matches!(self, IdentityError::Transport(_))
    }
}

/// Where a login interaction stopped. Returned by `MtprotoOps::sign_in` /
/// `check_password` and by the matching [`super::IdentityHandle`] calls so
/// the wizard can branch to the conditional 2FA step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    /// The login code was dispatched to the phone (Telegram delivery).
    CodeRequested,
    /// Fully signed in — the session is authorized.
    SignedIn,
    /// The account has 2FA enabled; `check_password` must follow.
    PasswordRequired,
}

/// A Telegram message normalized for the console/MCP surfaces: ids widened to
/// `i64`, dates as RFC 3339, sender as a display string. `date` stays a
/// string so this type never depends on a time crate at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedMessage {
    pub id: i64,
    /// RFC 3339 UTC timestamp.
    pub date: String,
    /// Display name, falling back to `@username` then the bare peer id.
    pub sender: String,
    pub text: String,
    /// Message this one replies to, when it is a reply.
    pub reply_to: Option<i64>,
}

/// A dialog line for the unread digest (`/monitor` + MCP `unread_digest`):
/// chats with pending activity, newest first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedDialog {
    /// Canonical handle usable in later requests: `@username` when public,
    /// otherwise the numeric peer id.
    pub chat: String,
    /// Display title (chat/channel/user name).
    pub title: String,
    pub unread: i64,
    /// Text of the most recent message (may be empty for service messages).
    pub last_text: String,
    /// RFC 3339 UTC timestamp of the most recent message, if any.
    pub last_date: Option<String>,
}

/// The MTProto surface the identity actor drives. `&mut self` methods keep
/// implementations free to hold login tokens and (re)build the underlying
/// client; all calls are issued strictly sequentially by the actor, so no
/// concurrency contract beyond `Send` is required.
pub(crate) trait MtprotoOps: Send {
    /// Send the login code to `phone`. `api_id`/`api_hash` come from the
    /// environment snapshot the actor holds; implementations may ignore
    /// `api_id` if their client was already constructed with it.
    fn request_login_code(
        &mut self,
        phone: &str,
        api_id: i32,
        api_hash: &str,
    ) -> impl Future<Output = Result<(), IdentityError>> + Send;

    /// Complete login with the received code. Returns
    /// [`LoginStep::PasswordRequired`] when the account has 2FA enabled (the
    /// implementation must retain the state needed by `check_password`).
    fn sign_in(
        &mut self,
        code: &str,
    ) -> impl Future<Output = Result<LoginStep, IdentityError>> + Send;

    /// Complete a 2FA login. Returns [`LoginStep::SignedIn`] on success.
    fn check_password(
        &mut self,
        password: &str,
    ) -> impl Future<Output = Result<LoginStep, IdentityError>> + Send;

    /// (Re)build the client from a persisted session snapshot (the decrypted
    /// `session` value of the identity blob) and verify it is authorized.
    fn connect_with_session(
        &mut self,
        session_json: &serde_json::Value,
    ) -> impl Future<Output = Result<(), IdentityError>> + Send;

    /// Serialize the live session for persistence. The value is an opaque
    /// snapshot (grammers `SessionData` fields); only the actor persists it
    /// and only `connect_with_session` ever consumes it.
    fn session_json(&self)
    -> impl Future<Output = Result<serde_json::Value, IdentityError>> + Send;

    /// Post `text` to `chat` (a `@username` or numeric peer id) as the
    /// personal account. Phase-1 write probe; the UI-facing error mapping to
    /// [`crate::identity::SendError`] happens in the actor.
    fn send_text(
        &mut self,
        chat: &str,
        text: &str,
    ) -> impl Future<Output = Result<crate::identity::SentMessage, IdentityError>> + Send;

    /// Up to `limit` (already clamped by the actor) messages from `chat`,
    /// newest first; `before` paginates with message-id exclusivity.
    fn history(
        &mut self,
        chat: &str,
        before: Option<i64>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<NormalizedMessage>, IdentityError>> + Send;

    /// Replies in the discussion thread rooted at `msg_id`.
    fn thread_replies(
        &mut self,
        chat: &str,
        msg_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<NormalizedMessage>, IdentityError>> + Send;

    /// Search messages in `chat` (required — the actor rejects chatless
    /// global search before this is reached; reads are scoped to explicitly
    /// configured channels, design decision 5).
    fn search(
        &mut self,
        query: &str,
        chat: &str,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<NormalizedMessage>, IdentityError>> + Send;

    /// Dialogs with unread activity (the unread digest), newest first.
    fn unread_digest(
        &mut self,
    ) -> impl Future<Output = Result<Vec<NormalizedDialog>, IdentityError>> + Send;

    /// Tear the client down (operator-requested disconnect). Idempotent.
    fn disconnect(&mut self) -> impl Future<Output = Result<(), IdentityError>> + Send;
}

/// Map a grammers `InvocationError` into [`IdentityError`], keeping the
/// taxonomies the design mandates discriminated: FloodWait (with its wait),
/// AuthKeyDuplicated (terminal), and everything else as `Transport` (the
/// string form of grammers errors never contains secrets, and RPC error
/// names are safe to surface).
pub(crate) fn map_invocation_error(err: &grammers_client::InvocationError) -> IdentityError {
    if err.is("FLOOD_WAIT") {
        let secs = rpc_error_value(err).unwrap_or(0) as u64;
        return IdentityError::FloodWait(secs);
    }
    if err.is("AUTH_KEY_DUPLICATED") {
        return IdentityError::AuthKeyDuplicated;
    }
    if err.is("SESSION_PASSWORD_NEEDED") {
        return IdentityError::PasswordRequired;
    }
    if err.is("PHONE_CODE_INVALID") || err.is("PHONE_CODE_EXPIRED") {
        return IdentityError::InvalidCode;
    }
    IdentityError::Transport(err.to_string())
}

/// The `value` field of the underlying `RpcError` (FloodWait's wait seconds),
/// when the invocation error is an RPC error.
fn rpc_error_value(err: &grammers_client::InvocationError) -> Option<u32> {
    if let grammers_client::InvocationError::Rpc(rpc) = err {
        rpc.value
    } else {
        None
    }
}

/// Mask a phone number for logs/status: keep the country prefix and the last
/// four digits, everything else becomes `•` (`+41•••1234`). Never called with
/// anything but a phone number; empty/short inputs collapse to `+•••`.
pub(crate) fn mask_phone(phone: &str) -> String {
    let trimmed = phone.trim();
    let digits: String = trimmed.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return "+•••".to_string();
    }
    let last4 = &digits[digits.len() - 4..];
    format!("+•••{last4}")
}
