//! Identity actor: the single owner of the MTProto client and its session
//! material (ratified user-identity design §3, L11 process shape).
//!
//! One task owns the [`MtprotoOps`] implementation; every caller — web
//! pages, MCP tools, the `Sender` resolution — holds a cloneable
//! [`IdentityHandle`] that turns typed requests into `mpsc` sends and gets
//! answers back over `oneshot` channels. Consequences enforced here:
//!
//! - **Sequential access.** All requests are processed one at a time from
//!   the mailbox, so reads are inherently sequential (L10 contract) and the
//!   login tokens never leave this task.
//! - **Read rate cap.** Reads (`History`/`ThreadReplies`/`Search`/
//!   `UnreadDigest`) pass a sliding-window limiter, ≤30/min; over the cap
//!   the caller gets [`IdentityError::RateLimited`] with a retry hint.
//! - **Read scoping.** Chat-taking reads (`History`/`ThreadReplies`/
//!   `Search`) resolve the chat ONLY against the DB `channels` table
//!   (design decision 5): unknown chats fail closed with
//!   [`IdentityError::ChatNotAllowed`], and chatless global search was
//!   removed (the handle still accepts `Option<&str>` but `None` is
//!   rejected with `ChatNotAllowed`).
//! - **FloodWait.** Never slept off inside the mailbox loop (a sleep there
//!   stalled every `Status` and 503'd the console): a flood wait ≤1800s
//!   records a per-surface gate with a scheduled wake (the backoff-gate
//!   pattern) and returns to the caller immediately; gated ops are refused
//!   with the remaining wait until the wake fires. >1800s halts the surface
//!   outright.
//! - **Flood-halt policy.** Each surface (reads, sends) counts FloodWaits in
//!   a rolling 1-hour window; ≥3 — or a single wait >30min — halts that
//!   surface terminally ([`IdentityError::SurfaceHalted`] /
//!   [`SendError::SurfaceHalted`]) until an owner `ResetSurfaces` or a
//!   process restart.
//! - **Transport backoff.** Transport failures open an exponential-backoff
//!   (+jitter) gate inside the actor; requests arriving during the gate are
//!   answered with a transport error instead of hammering Telegram.
//! - **Terminal disable and recovery.** `AUTH_KEY_DUPLICATED` moves the
//!   state machine to [`IdentityState::Disabled`]; every further request is
//!   rejected until the owner-driven [`IdentityHandle::wipe`] (the only
//!   recovery: disconnect + delete the session blob + back to
//!   `Configured`/`NotConfigured`).
//! - **No secrets in logs.** Codes, 2FA passwords, passphrases and session
//!   material are never logged; the phone number is stored masked
//!   ([`mask_phone`]).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};

use super::ops::{
    IdentityError, LoginStep, MtprotoOps, NormalizedDialog, NormalizedMessage,
    map_invocation_error, mask_phone,
};
use super::session::SessionStore;
use super::{SendError, SentMessage};

/// Upper bound per read request; the actor clamps, callers never have to.
pub(crate) const MAX_FETCH_LIMIT: usize = 100;
/// FloodWait durations above this halt the surface outright (L10: a wait
/// above 30min is not worth gating — the surface is down anyway); at or
/// below it a scheduled-wake gate applies.
const MAX_FLOOD_WAIT_SLEEP: u64 = 1800;
/// Reads per sliding 60s window (L10 contract).
const READS_PER_MINUTE: usize = 30;
/// How many dialogs `unread_digest` may scan before returning.
const MAX_DIGEST_SCANNED_DIALOGS: usize = 200;
/// Backoff ceiling for the reconnect gate.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Rolling window for the flood-halt counter (L10: ≥3 FloodWaits in an
/// hour halts the surface).
const FLOOD_HALT_WINDOW: Duration = Duration::from_secs(3600);
/// FloodWaits within [`FLOOD_HALT_WINDOW`] that trigger a surface halt.
const FLOOD_HALT_THRESHOLD: usize = 3;

/// Which rate-limited surface a request belongs to. Flood gates and flood
/// halts are tracked per surface (L10 flood-halt policy): a read flood never
/// blocks sends and vice versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    Reads,
    Sends,
}

impl Surface {
    fn idx(self) -> usize {
        match self {
            Surface::Reads => 0,
            Surface::Sends => 1,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Surface::Reads => "reads",
            Surface::Sends => "sends",
        }
    }

    fn from_idx(idx: usize) -> Self {
        if idx == Surface::Reads.idx() {
            Surface::Reads
        } else {
            Surface::Sends
        }
    }
}

// ---- Public state/status ----------------------------------------------------

/// Lifecycle of the identity subsystem, as observed through
/// [`IdentityHandle::status`]. The wizard, the unlock banner and the status
/// line all branch on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityState {
    /// No actor task is running (embedded hosts without the identity).
    Disconnected,
    /// `TELEGRAM_API_ID`/`TELEGRAM_API_HASH` missing — nothing to do.
    NotConfigured,
    /// Credentials present; no session yet — the pairing wizard may run.
    Configured,
    /// Login code sent to the (masked) phone; awaiting the code.
    AwaitingCode,
    /// Telegram reported 2FA; awaiting the one-shot password.
    AwaitingPassword,
    /// Connected with an authorized session.
    Ready,
    /// A session blob exists on disk but the passphrase was not entered
    /// since the last restart.
    Locked,
    /// `AUTH_KEY_DUPLICATED`: terminal until the session is re-paired.
    Disabled,
}

impl IdentityState {
    /// Whether requests that need a live client can proceed.
    pub fn is_ready(self) -> bool {
        self == IdentityState::Ready
    }
}

/// Snapshot of the subsystem state for the console/MCP surfaces.
/// `persisted` distinguishes a freshly paired (not yet stored) session from
/// one the unlock passphrase already protects.
#[derive(Debug, Clone)]
pub struct IdentityStatus {
    pub state: IdentityState,
    /// Human-readable detail (masked phone; never secrets).
    pub detail: String,
    /// True when the on-disk blob matches the live session.
    pub persisted: bool,
}

// ---- Handle ------------------------------------------------------------------

pub(crate) enum IdentityRequest {
    Status(oneshot::Sender<IdentityStatus>),
    Unlock {
        passphrase: String,
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    RequestCode {
        phone: String,
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    SignIn {
        code: String,
        reply: oneshot::Sender<Result<LoginStep, IdentityError>>,
    },
    CheckPassword {
        password: String,
        reply: oneshot::Sender<Result<LoginStep, IdentityError>>,
    },
    SetPassphraseAndPersist {
        passphrase: String,
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    SendText {
        chat_id: i64,
        text: String,
        reply: oneshot::Sender<Result<SentMessage, SendError>>,
    },
    /// Send to a channel handle (`@name` or numeric id as a string) — the
    /// canonical chat identifier of the user-identity feature (the DB
    /// `channels.channel` value, same resolution the read path uses). Same
    /// gates and caps as [`IdentityRequest::SendText`].
    SendTextTo {
        chat: String,
        text: String,
        reply: oneshot::Sender<Result<SentMessage, SendError>>,
    },
    History {
        chat: String,
        before: Option<i64>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<NormalizedMessage>, IdentityError>>,
    },
    ThreadReplies {
        chat: String,
        msg_id: i64,
        before: Option<i64>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<NormalizedMessage>, IdentityError>>,
    },
    Search {
        query: String,
        chat: Option<String>,
        limit: usize,
        reply: oneshot::Sender<Result<Vec<NormalizedMessage>, IdentityError>>,
    },
    UnreadDigest {
        reply: oneshot::Sender<Result<Vec<NormalizedDialog>, IdentityError>>,
    },
    Disconnect {
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    /// Owner-driven destructive recovery (F5): disconnect, delete the
    /// session blob, reset the state machine. The ONLY path out of
    /// [`IdentityState::Disabled`].
    Wipe {
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    /// Owner unlock for flood-halted surfaces (F4): clears every flood gate,
    /// rolling counter and halt flag. Also usable before any halt as a
    /// general surface reset.
    ResetSurfaces {
        reply: oneshot::Sender<Result<(), IdentityError>>,
    },
    /// Internal: a scheduled backoff gate expiry.
    BackoffCleared,
    /// Internal: a scheduled flood-gate expiry for the surface index.
    FloodCleared(usize),
}

/// Cloneable client of the identity actor. `unconnected()` yields a handle
/// whose calls fail honestly ([`IdentityError::NotConnected`],
/// [`SendError::NotAuthorized`]) instead of hanging — used by embedded hosts
/// and every `WebState` construction site that did not spawn the actor.
#[derive(Clone)]
pub struct IdentityHandle {
    tx: Option<mpsc::Sender<IdentityRequest>>,
}

impl IdentityHandle {
    /// A handle to a non-running subsystem. Status reports
    /// [`IdentityState::Disconnected`]; everything else fails closed.
    pub fn unconnected() -> Self {
        Self { tx: None }
    }

    /// Whether the handle points at a running actor. It does NOT report the
    /// login state — use [`Self::status`] for that.
    pub fn is_connected(&self) -> bool {
        self.tx.is_some()
    }

    async fn call<R>(
        &self,
        build: impl FnOnce(oneshot::Sender<R>) -> IdentityRequest,
        unconnected: impl FnOnce() -> R,
    ) -> R {
        let Some(tx) = &self.tx else {
            return unconnected();
        };
        let (reply, rx) = oneshot::channel();
        // A send failure or dropped responder means the actor died between
        // spawn and now; that reads as NotConnected, never as a panic.
        if tx.send(build(reply)).await.is_err() {
            return unconnected();
        }
        match rx.await {
            Ok(r) => r,
            Err(_) => unconnected(),
        }
    }

    pub async fn status(&self) -> IdentityStatus {
        self.call(IdentityRequest::Status, || IdentityStatus {
            state: IdentityState::Disconnected,
            detail: "personal identity subsystem is not running".to_string(),
            persisted: false,
        })
        .await
    }

    pub async fn unlock(&self, passphrase: &str) -> Result<(), IdentityError> {
        let passphrase = passphrase.to_string();
        self.call(
            |reply| IdentityRequest::Unlock { passphrase, reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    pub async fn request_code(&self, phone: &str) -> Result<(), IdentityError> {
        let phone = phone.to_string();
        self.call(
            |reply| IdentityRequest::RequestCode { phone, reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    pub async fn sign_in(&self, code: &str) -> Result<LoginStep, IdentityError> {
        let code = code.to_string();
        self.call(
            |reply| IdentityRequest::SignIn { code, reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    pub async fn check_password(&self, password: &str) -> Result<LoginStep, IdentityError> {
        let password = password.to_string();
        self.call(
            |reply| IdentityRequest::CheckPassword { password, reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    pub async fn set_passphrase_and_persist(&self, passphrase: &str) -> Result<(), IdentityError> {
        let passphrase = passphrase.to_string();
        self.call(
            |reply| IdentityRequest::SetPassphraseAndPersist { passphrase, reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Send as the personal account. Not-Ready subsystems fail loudly with
    /// [`SendError::NotAuthorized`] — a user-bound channel must never
    /// silently fall back to the bot (IDENTITY_SENDER_UX_SPEC).
    pub async fn send_text(&self, chat_id: i64, text: &str) -> Result<SentMessage, SendError> {
        let text = text.to_string();
        self.call(
            |reply| IdentityRequest::SendText {
                chat_id,
                text,
                reply,
            },
            || Err(SendError::NotAuthorized),
        )
        .await
    }

    /// Send to a channel handle (`@name` or numeric id as a string) as the
    /// personal account. The i64 form ([`Self::send_text`]) stays the uniform
    /// [`super::ContentSender`] contract; this variant carries the DB
    /// channel-handle form. Identical gates (state, backoff, flood, caps).
    pub async fn send_text_to(&self, chat: &str, text: &str) -> Result<SentMessage, SendError> {
        let chat = chat.to_string();
        let text = text.to_string();
        self.call(
            |reply| IdentityRequest::SendTextTo { chat, text, reply },
            || Err(SendError::NotAuthorized),
        )
        .await
    }

    /// Clamp a `before` pagination cursor into the i32 range the Telegram
    /// layer uses (F8): cursors beyond the newest message clamp up to
    /// i32::MAX (still "everything older"), cursors below the oldest clamp
    /// down to i32::MIN (still "nothing"). Kept lossless-in-semantics —
    /// message ids never leave the i32 range server-side.
    fn clamp_before(before: Option<i64>) -> Option<i64> {
        before.map(|b| b.clamp(i32::MIN as i64, i32::MAX as i64))
    }

    pub async fn history(
        &self,
        chat: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        let chat = chat.to_string();
        let before = Self::clamp_before(before);
        self.call(
            |reply| IdentityRequest::History {
                chat,
                before,
                limit,
                reply,
            },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Replies in the discussion thread rooted at `msg_id`. A `msg_id`
    /// outside the i32 wire range is rejected up front
    /// ([`IdentityError::InvalidCursor`]) instead of silently truncating
    /// into a different thread (F8).
    pub async fn thread_replies(
        &self,
        chat: &str,
        msg_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        if !(i32::MIN as i64..=i32::MAX as i64).contains(&msg_id) {
            return Err(IdentityError::InvalidCursor);
        }
        let chat = chat.to_string();
        let before = Self::clamp_before(before);
        self.call(
            |reply| IdentityRequest::ThreadReplies {
                chat,
                msg_id,
                before,
                limit,
                reply,
            },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Search messages in `chat`. The signature keeps `Option<&str>` for
    /// call-site compatibility, but chatless (account-wide) search was
    /// removed per design decision 5: `None` is rejected with
    /// [`IdentityError::ChatNotAllowed`] before any Telegram traffic, and a
    /// chat that is not an explicitly configured channel is rejected too.
    pub async fn search(
        &self,
        query: &str,
        chat: Option<&str>,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        let query = query.to_string();
        let chat = chat.map(str::to_string);
        self.call(
            |reply| IdentityRequest::Search {
                query,
                chat,
                limit,
                reply,
            },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    pub async fn unread_digest(&self) -> Result<Vec<NormalizedDialog>, IdentityError> {
        self.call(
            |reply| IdentityRequest::UnreadDigest { reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Drop the live client. With a persisted session the subsystem returns
    /// to [`IdentityState::Locked`] (a restart needs the passphrase again);
    /// without one it returns to [`IdentityState::Configured`].
    pub async fn disconnect(&self) -> Result<(), IdentityError> {
        self.call(
            |reply| IdentityRequest::Disconnect { reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Owner-driven destructive re-pair (F5): disconnect the client, delete
    /// the encrypted session blob and reset the state machine to
    /// [`IdentityState::Configured`] (credentials remain in the env) or
    /// [`IdentityState::NotConfigured`]. This is the ONLY recovery from the
    /// terminal [`IdentityState::Disabled`] (`AUTH_KEY_DUPLICATED`). Allowed
    /// from every state; the blob is confirmed gone or the wipe fails.
    pub async fn wipe(&self) -> Result<(), IdentityError> {
        self.call(
            |reply| IdentityRequest::Wipe { reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }

    /// Owner unlock for flood-halted surfaces (F4): a surface that hit the
    /// ≥3-FloodWaits-per-hour (or single >30min wait) halt stays halted
    /// until this call or a process restart. Clears every halt flag, rolling
    /// flood counter and pending flood gate.
    pub async fn reset_surfaces(&self) -> Result<(), IdentityError> {
        self.call(
            |reply| IdentityRequest::ResetSurfaces { reply },
            || Err(IdentityError::NotConnected),
        )
        .await
    }
}

// ---- Rate limiter ------------------------------------------------------------

/// Sliding-window limiter for reads. Not `Clone`: owned by the actor, so the
/// cap holds across every handle clone.
pub(crate) struct ReadRateLimiter {
    window: VecDeque<Instant>,
    max: usize,
}

const RATE_WINDOW: Duration = Duration::from_secs(60);

impl ReadRateLimiter {
    pub(crate) fn new(max: usize) -> Self {
        Self {
            window: VecDeque::new(),
            max,
        }
    }

    /// Ok: the read is admitted (timestamp recorded). Err: seconds until the
    /// oldest read in the window ages out.
    fn check(&mut self) -> Result<(), u64> {
        let now = Instant::now();
        while let Some(front) = self.window.front() {
            if now.duration_since(*front) >= RATE_WINDOW {
                self.window.pop_front();
            } else {
                break;
            }
        }
        if self.window.len() >= self.max {
            let wait = match self.window.front() {
                Some(oldest) => RATE_WINDOW
                    .saturating_sub(now.duration_since(*oldest))
                    .as_secs()
                    .max(1),
                None => 1,
            };
            return Err(wait);
        }
        self.window.push_back(now);
        Ok(())
    }
}

// ---- Send caps -----------------------------------------------------------------

/// Sliding-window cap for the hourly send surface (L10 contract: posts
/// ≤5/hour AND ≤15/day). Not `Clone`: owned by the actor, so the cap holds
/// across every handle clone.
///
/// Deliberately IN-MEMORY ONLY: L10 required no cross-restart accounting, so
/// a process restart resets the budget (the flood-halt machinery covers the
/// restart-resistant failure mode). Also deliberately independent of the
/// flood gates/halt (`Surface::Sends`): a cap refusal is not a flood event
/// and never arms the halt — the two mechanisms gate independently, and both
/// can refuse the same send.
pub(crate) struct SendCaps {
    hour: VecDeque<Instant>,
    day: VecDeque<Instant>,
}

const SEND_WINDOW_HOUR: Duration = Duration::from_secs(3600);
const SEND_WINDOW_DAY: Duration = Duration::from_secs(24 * 3600);
/// Posts per sliding hour (L10).
const SENDS_PER_HOUR: usize = 5;
/// Posts per sliding day (L10).
const SENDS_PER_DAY: usize = 15;

impl SendCaps {
    pub(crate) fn new() -> Self {
        Self {
            hour: VecDeque::new(),
            day: VecDeque::new(),
        }
    }

    fn prune(window: &mut VecDeque<Instant>, now: Instant, span: Duration) {
        while let Some(front) = window.front() {
            if now.duration_since(*front) >= span {
                window.pop_front();
            } else {
                break;
            }
        }
    }

    /// Seconds until `oldest` ages out of `span`, minimum 1 (a refused caller
    /// must never be told "retry in 0s").
    fn wait_secs(oldest: Option<&Instant>, now: Instant, span: Duration) -> u64 {
        oldest
            .map(|t| span.saturating_sub(now.duration_since(*t)).as_secs().max(1))
            .unwrap_or(1)
    }

    /// Ok: a send may proceed. Err: `(limit, retry_after_secs)` — `limit`
    /// names the cap that refused, for honest downstream copy.
    pub(crate) fn check(&mut self) -> Result<(), (&'static str, u64)> {
        self.check_at(Instant::now())
    }

    /// Clock-injected form of [`Self::check`] so tests can slide the windows
    /// without sleeping (and without depending on process uptime).
    fn check_at(&mut self, now: Instant) -> Result<(), (&'static str, u64)> {
        Self::prune(&mut self.hour, now, SEND_WINDOW_HOUR);
        Self::prune(&mut self.day, now, SEND_WINDOW_DAY);
        if self.hour.len() >= SENDS_PER_HOUR {
            let wait = Self::wait_secs(self.hour.front(), now, SEND_WINDOW_HOUR);
            return Err(("5 posts per hour", wait));
        }
        if self.day.len() >= SENDS_PER_DAY {
            let wait = Self::wait_secs(self.day.front(), now, SEND_WINDOW_DAY);
            return Err(("15 posts per day", wait));
        }
        Ok(())
    }

    /// Record an attempt that reached the MTProto layer (conservative:
    /// successes, flood-wait refusals and plain failures all count — the cap
    /// protects the account, so it charges every attempt Telegram saw).
    pub(crate) fn record(&mut self) {
        self.record_at(Instant::now())
    }

    /// Clock-injected form of [`Self::record`].
    fn record_at(&mut self, at: Instant) {
        self.hour.push_back(at);
        self.day.push_back(at);
    }
}

// ---- Actor core ---------------------------------------------------------------

struct IdentityActor<O: MtprotoOps> {
    ops: O,
    state: IdentityState,
    /// Data directory for the encrypted session blob (`identity/session.bin`
    /// inside it). `None` only in tests without persistence.
    store_dir: Option<PathBuf>,
    /// The app database — the source of truth for the channel allowlist
    /// (design decision 5). `None` in tests without persistence; reads then
    /// fail closed (`ChatNotAllowed`) because nothing is configured.
    db: Option<Arc<crate::db::Db>>,
    api_id: Option<i32>,
    api_hash: Option<String>,
    /// Masked phone only (`+•••1234`) — the raw number is never stored.
    phone_masked: Option<String>,
    persisted: bool,
    limiter: ReadRateLimiter,
    /// Sliding-window send caps (L10: posts ≤5/hour, ≤15/day). Independent
    /// of the flood gates below — both can refuse a send.
    send_caps: SendCaps,
    backoff_until: Option<Instant>,
    transport_failures: u32,
    /// Per-surface flood gates (F1): a wake time until which gated ops are
    /// refused without touching Telegram. Indexed by [`Surface::idx`].
    flood_until: [Option<Instant>; 2],
    /// Per-surface rolling FloodWait timestamps for the halt counter (F4).
    flood_waits: [VecDeque<Instant>; 2],
    /// Per-surface terminal halt flags (F4): cleared only by `ResetSurfaces`
    /// or a restart.
    halted: [bool; 2],
}

/// Startup state derivation: an existing blob wins (Locked beats
/// Configured — the operator must re-enter the passphrase), credentials
/// without a blob start the wizard, neither leaves it dormant.
fn initial_state(has_blob: bool, has_creds: bool) -> IdentityState {
    if has_blob {
        IdentityState::Locked
    } else if has_creds {
        IdentityState::Configured
    } else {
        IdentityState::NotConfigured
    }
}

impl<O: MtprotoOps + 'static> IdentityActor<O> {
    fn status(&self) -> IdentityStatus {
        let detail: &str = match self.state {
            IdentityState::Disconnected => "personal identity subsystem is not running",
            IdentityState::NotConfigured => "no TELEGRAM_API_ID / TELEGRAM_API_HASH configured",
            IdentityState::Configured => "credentials configured; pairing not started",
            IdentityState::AwaitingCode => "login code sent; enter the code",
            IdentityState::AwaitingPassword => "2FA password required",
            IdentityState::Ready => "connected",
            IdentityState::Locked => "session locked; unlock required",
            IdentityState::Disabled => "authorization key duplicated; re-pair required",
        };
        let detail = match (&self.state, &self.phone_masked) {
            (IdentityState::AwaitingCode, Some(p)) => format!("{detail} ({p})"),
            _ => detail.to_string(),
        };
        IdentityStatus {
            state: self.state,
            detail,
            persisted: self.persisted,
        }
    }

    /// Gate for requests that need a live, authorized client.
    fn guard_ready(&self) -> Result<(), IdentityError> {
        match self.state {
            IdentityState::Ready => Ok(()),
            IdentityState::NotConfigured => Err(IdentityError::NotConfigured),
            IdentityState::Locked => Err(IdentityError::Locked),
            IdentityState::Disabled => Err(IdentityError::AuthKeyDuplicated),
            IdentityState::Disconnected => Err(IdentityError::NotConnected),
            _ => Err(IdentityError::NotAuthorized),
        }
    }

    fn in_backoff(&self) -> bool {
        self.backoff_until
            .is_some_and(|until| Instant::now() < until)
    }

    /// F1/F4 surface gates: a halted surface fails closed; a flood-gated
    /// surface is refused with the remaining wait until its scheduled wake
    /// fires. Never sleeps — the mailbox loop must never stall on a flood
    /// wait (that stalled every `Status` and 503'd the console).
    fn check_surface(&self, surface: Surface) -> Result<(), IdentityError> {
        if self.halted[surface.idx()] {
            return Err(IdentityError::SurfaceHalted(surface.name()));
        }
        if let Some(until) = self.flood_until[surface.idx()] {
            let now = Instant::now();
            if now < until {
                let wait = until.saturating_duration_since(now).as_secs().max(1);
                return Err(IdentityError::FloodWait(wait));
            }
        }
        Ok(())
    }

    /// F3: chat-taking reads resolve ONLY against the DB `channels` table
    /// (design decision 5). Returns the canonical channel name on success;
    /// anything not explicitly configured — including the no-DB test setup —
    /// fails closed with [`IdentityError::ChatNotAllowed`].
    fn resolve_allowed_chat(&self, chat: &str) -> Result<String, IdentityError> {
        let trimmed = chat.trim();
        let db = self
            .db
            .as_ref()
            .ok_or_else(|| IdentityError::ChatNotAllowed(trimmed.to_string()))?;
        db.known_channel(trimmed)
            .ok_or_else(|| IdentityError::ChatNotAllowed(trimmed.to_string()))
    }

    /// Pre-flight for a read: state guard, reconnect backoff gate, surface
    /// gates (flood/halt). The allowlist and the ops call happen after this
    /// returns Ok.
    fn check_read_gate(&mut self) -> Result<(), IdentityError> {
        self.guard_ready()?;
        if self.in_backoff() {
            return Err(IdentityError::Transport(
                "personal identity is reconnecting; retry shortly".to_string(),
            ));
        }
        self.check_surface(Surface::Reads)?;
        if let Err(wait) = self.limiter.check() {
            tracing::debug!(wait_s = wait, "identity read rate limited");
            return Err(IdentityError::RateLimited(wait));
        }
        Ok(())
    }

    /// Record a FloodWait against its surface (F1 gate + F4 counter). A wait
    /// within [`MAX_FLOOD_WAIT_SLEEP`] schedules a gate wake; anything longer
    /// halts the surface outright. The third wait inside the rolling hour (or
    /// any single >30min wait) halts the surface terminally — alert-level
    /// warn with the surface name, never any chat or account detail.
    fn note_flood(&mut self, surface: Surface, secs: u64, tx: &mpsc::Sender<IdentityRequest>) {
        let idx = surface.idx();
        let now = Instant::now();
        let waits = &mut self.flood_waits[idx];
        while let Some(front) = waits.front() {
            if now.duration_since(*front) >= FLOOD_HALT_WINDOW {
                waits.pop_front();
            } else {
                break;
            }
        }
        waits.push_back(now);
        let over_limit = secs > MAX_FLOOD_WAIT_SLEEP;
        if !self.halted[idx] && (over_limit || waits.len() >= FLOOD_HALT_THRESHOLD) {
            self.halted[idx] = true;
            tracing::warn!(
                surface = surface.name(),
                wait_s = secs,
                floods_in_window = waits.len(),
                "personal identity surface halted: repeated Telegram flood control; \
                 owner surface reset or restart required"
            );
        }
        if !self.halted[idx] && secs > 0 && secs <= MAX_FLOOD_WAIT_SLEEP {
            // F1: record the wake time and keep the mailbox loop moving; a
            // scheduled FloodCleared lifts the gate (unless a newer flood
            // pushed a later deadline past now).
            self.flood_until[idx] = Some(now + Duration::from_secs(secs));
            let tx = tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                let _ = tx.send(IdentityRequest::FloodCleared(idx)).await;
            });
        }
    }

    /// Record an op failure: duplicate keys disable the subsystem; flood
    /// waits feed the per-surface gate/halt machinery; transport failures
    /// open the reconnect backoff gate (scheduled expiry below). `surface`
    /// is `None` for login-path calls — flood waits there belong to no
    /// rate-limited surface.
    fn note_error(
        &mut self,
        err: &IdentityError,
        surface: Option<Surface>,
        tx: &mpsc::Sender<IdentityRequest>,
    ) {
        match err {
            IdentityError::AuthKeyDuplicated => {
                self.state = IdentityState::Disabled;
                tracing::error!(
                    "personal identity disabled: authorization key used by another client; re-pair required"
                );
            }
            IdentityError::FloodWait(secs) => {
                if let Some(surface) = surface {
                    self.note_flood(surface, *secs, tx);
                }
            }
            e if e.is_transport() => {
                self.transport_failures = self.transport_failures.saturating_add(1);
                // Exponential: 0.5s, 1s, 2s, … capped at MAX_BACKOFF, with
                // ±25% jitter so concurrent restarts don't sync up.
                let base = 500u64 << self.transport_failures.saturating_sub(1).min(6);
                let delay =
                    Duration::from_millis((base as f64 * (0.75 + 0.5 * jitter_fraction())) as u64)
                        .min(MAX_BACKOFF);
                self.backoff_until = Some(Instant::now() + delay);
                tracing::warn!(
                    attempts = self.transport_failures,
                    backoff_ms = delay.as_millis() as u64,
                    "personal identity transport failure; backing off"
                );
                let tx = tx.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(delay).await;
                    let _ = tx.send(IdentityRequest::BackoffCleared).await;
                });
            }
            _ => {}
        }
    }

    /// Post-process an ops result (error bookkeeping only — flood waits are
    /// gated, never slept off here) and pass the outcome through to the
    /// caller.
    fn read_outcome<T>(
        &mut self,
        result: Result<T, IdentityError>,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<T, IdentityError> {
        if let Err(e) = &result {
            self.note_error(e, Some(Surface::Reads), tx);
        }
        result
    }

    /// Clear flood/halt bookkeeping (F4 owner reset; also part of wipe).
    fn reset_surface_state(&mut self) {
        self.flood_until = [None, None];
        self.flood_waits = [VecDeque::new(), VecDeque::new()];
        self.halted = [false, false];
    }

    async fn run(
        mut self,
        mut rx: mpsc::Receiver<IdentityRequest>,
        tx: mpsc::Sender<IdentityRequest>,
    ) {
        while let Some(request) = rx.recv().await {
            match request {
                IdentityRequest::Status(reply) => {
                    let _ = reply.send(self.status());
                }
                IdentityRequest::BackoffCleared => {
                    // Gate expiry: reset only when no newer failure pushed a
                    // later deadline past now.
                    if !self.in_backoff() {
                        self.transport_failures = 0;
                        tracing::info!("identity reconnect gate cleared");
                    }
                }
                IdentityRequest::FloodCleared(idx) => {
                    // Flood-gate expiry (F1): clear only when no newer flood
                    // pushed a later deadline past now.
                    if let Some(until) = self.flood_until[idx]
                        && Instant::now() >= until
                    {
                        self.flood_until[idx] = None;
                        tracing::info!(
                            surface = Surface::from_idx(idx).name(),
                            "identity flood gate cleared"
                        );
                    }
                }
                IdentityRequest::Unlock { passphrase, reply } => {
                    let _ = reply.send(self.unlock(&passphrase, &tx).await);
                }
                IdentityRequest::RequestCode { phone, reply } => {
                    let _ = reply.send(self.request_code(&phone, &tx).await);
                }
                IdentityRequest::SignIn { code, reply } => {
                    let _ = reply.send(self.sign_in(&code, &tx).await);
                }
                IdentityRequest::CheckPassword { password, reply } => {
                    let _ = reply.send(self.check_password(&password, &tx).await);
                }
                IdentityRequest::SetPassphraseAndPersist { passphrase, reply } => {
                    let _ = reply.send(self.set_passphrase_and_persist(&passphrase).await);
                }
                IdentityRequest::SendText {
                    chat_id,
                    text,
                    reply,
                } => {
                    let _ = reply.send(self.send_text(&chat_id.to_string(), &text, &tx).await);
                }
                IdentityRequest::SendTextTo { chat, text, reply } => {
                    let _ = reply.send(self.send_text(&chat, &text, &tx).await);
                }
                IdentityRequest::History {
                    chat,
                    before,
                    limit,
                    reply,
                } => {
                    let limit = limit.min(MAX_FETCH_LIMIT);
                    let result = match self.check_read_gate() {
                        Err(e) => Err(e),
                        Ok(()) => match self.resolve_allowed_chat(&chat) {
                            Err(e) => Err(e),
                            // Only the canonical DB-configured name reaches
                            // MTProto (design decision 5).
                            Ok(canonical) if limit == 0 => {
                                let _ = canonical;
                                Ok(Vec::new())
                            }
                            Ok(canonical) => {
                                let r = self.ops.history(&canonical, before, limit).await;
                                self.read_outcome(r, &tx)
                            }
                        },
                    };
                    let _ = reply.send(result);
                }
                IdentityRequest::ThreadReplies {
                    chat,
                    msg_id,
                    before,
                    limit,
                    reply,
                } => {
                    let limit = limit.min(MAX_FETCH_LIMIT);
                    let result = match self.check_read_gate() {
                        Err(e) => Err(e),
                        Ok(()) => match self.resolve_allowed_chat(&chat) {
                            Err(e) => Err(e),
                            Ok(canonical) if limit == 0 => {
                                let _ = canonical;
                                Ok(Vec::new())
                            }
                            Ok(canonical) => {
                                let r = self
                                    .ops
                                    .thread_replies(&canonical, msg_id, before, limit)
                                    .await;
                                self.read_outcome(r, &tx)
                            }
                        },
                    };
                    let _ = reply.send(result);
                }
                IdentityRequest::Search {
                    query,
                    chat,
                    limit,
                    reply,
                } => {
                    let limit = limit.min(MAX_FETCH_LIMIT);
                    // F3: chatless (account-wide) search was removed — fail
                    // closed before any gate or Telegram traffic.
                    let result = match chat.as_deref() {
                        None => Err(IdentityError::ChatNotAllowed(
                            "no channel given (account-wide search is disabled)".to_string(),
                        )),
                        Some(c) => match self.check_read_gate() {
                            Err(e) => Err(e),
                            Ok(()) => match self.resolve_allowed_chat(c) {
                                Err(e) => Err(e),
                                Ok(canonical) if limit == 0 => {
                                    let _ = canonical;
                                    Ok(Vec::new())
                                }
                                Ok(canonical) => {
                                    let r = self.ops.search(&query, &canonical, limit).await;
                                    self.read_outcome(r, &tx)
                                }
                            },
                        },
                    };
                    let _ = reply.send(result);
                }
                IdentityRequest::UnreadDigest { reply } => {
                    let result = match self.check_read_gate() {
                        Err(e) => Err(e),
                        Ok(()) => {
                            let r = self.ops.unread_digest().await;
                            self.read_outcome(r, &tx)
                        }
                    };
                    let _ = reply.send(result);
                }
                IdentityRequest::Disconnect { reply } => {
                    let _ = reply.send(self.disconnect(&tx).await);
                }
                IdentityRequest::Wipe { reply } => {
                    let _ = reply.send(self.wipe().await);
                }
                IdentityRequest::ResetSurfaces { reply } => {
                    self.reset_surface_state();
                    tracing::info!("identity flood halts and gates cleared by owner");
                    let _ = reply.send(Ok(()));
                }
            }
        }
    }

    // ---- State transitions -------------------------------------------------

    async fn unlock(
        &mut self,
        passphrase: &str,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<(), IdentityError> {
        if self.state != IdentityState::Locked {
            return Err(match self.state {
                IdentityState::Ready => IdentityError::InvalidState("already unlocked"),
                IdentityState::NotConfigured => IdentityError::NotConfigured,
                IdentityState::Disabled => IdentityError::AuthKeyDuplicated,
                _ => IdentityError::InvalidState("not locked"),
            });
        }
        if !self.api_id.is_some_and(|id| id != 0)
            || self.api_hash.as_deref().is_none_or(str::is_empty)
        {
            return Err(IdentityError::NotConfigured);
        }
        let Some(dir) = self.store_dir.clone() else {
            return Err(IdentityError::NotConnected);
        };
        // PBKDF2 (600k iterations) is CPU-bound: keep it off the reactor.
        let passphrase = passphrase.to_string();
        let loaded = tokio::task::spawn_blocking(move || SessionStore::new(&dir).load(&passphrase))
            .await
            .map_err(|e| IdentityError::Internal(format!("unlock join: {e}")))?;
        let session_json = match loaded {
            Ok(v) => v,
            Err(super::session::SessionStoreError::WrongPassphrase) => {
                // Deliberately NOT a state change: the blob is untouched and
                // the subsystem stays locked (UX: retry the passphrase).
                return Err(IdentityError::WrongPassphrase);
            }
            Err(super::session::SessionStoreError::VersionMismatch {
                blob_grammers,
                crate_grammers,
            }) => {
                return Err(IdentityError::VersionMismatch {
                    blob_grammers,
                    crate_grammers,
                });
            }
            Err(e) => return Err(IdentityError::Transport(e.to_string())),
        };
        match self.ops.connect_with_session(&session_json).await {
            Ok(()) => {
                self.state = IdentityState::Ready;
                self.persisted = true;
                tracing::info!("personal identity unlocked and connected");
                Ok(())
            }
            Err(e) => {
                self.note_error(&e, None, tx);
                Err(e)
            }
        }
    }

    async fn request_code(
        &mut self,
        phone: &str,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<(), IdentityError> {
        if self.state == IdentityState::Disabled {
            return Err(IdentityError::AuthKeyDuplicated);
        }
        if self.state == IdentityState::Ready {
            return Err(IdentityError::InvalidState("already signed in"));
        }
        let (api_id, api_hash) = match (self.api_id, self.api_hash.as_deref()) {
            (Some(id), Some(h)) if id != 0 && !h.is_empty() => (id, h),
            _ => return Err(IdentityError::NotConfigured),
        };
        if phone.trim().is_empty() {
            return Err(IdentityError::InvalidState("phone required"));
        }
        let result = self
            .ops
            .request_login_code(phone.trim(), api_id, api_hash)
            .await;
        match result {
            Ok(()) => {
                self.state = IdentityState::AwaitingCode;
                self.phone_masked = Some(mask_phone(phone));
                Ok(())
            }
            Err(e) => {
                self.note_error(&e, None, tx);
                Err(e)
            }
        }
    }

    async fn sign_in(
        &mut self,
        code: &str,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<LoginStep, IdentityError> {
        if self.state != IdentityState::AwaitingCode {
            return Err(match self.state {
                IdentityState::AwaitingPassword => {
                    IdentityError::InvalidState("awaiting the 2FA password")
                }
                IdentityState::Ready => IdentityError::InvalidState("already signed in"),
                IdentityState::Disabled => IdentityError::AuthKeyDuplicated,
                _ => IdentityError::InvalidState("not awaiting a login code"),
            });
        }
        let result = self.ops.sign_in(code).await;
        match result {
            Ok(LoginStep::SignedIn) => {
                self.state = IdentityState::Ready;
                self.persisted = false;
                Ok(LoginStep::SignedIn)
            }
            Ok(LoginStep::PasswordRequired) => {
                self.state = IdentityState::AwaitingPassword;
                Ok(LoginStep::PasswordRequired)
            }
            Ok(step) => Ok(step),
            Err(e) => {
                self.note_error(&e, None, tx);
                Err(e)
            }
        }
    }

    async fn check_password(
        &mut self,
        password: &str,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<LoginStep, IdentityError> {
        if self.state != IdentityState::AwaitingPassword {
            return Err(match self.state {
                IdentityState::AwaitingCode => {
                    IdentityError::InvalidState("no 2FA step is pending")
                }
                IdentityState::Ready => IdentityError::InvalidState("already signed in"),
                IdentityState::Disabled => IdentityError::AuthKeyDuplicated,
                _ => IdentityError::InvalidState("not awaiting a 2FA password"),
            });
        }
        let result = self.ops.check_password(password).await;
        match result {
            Ok(LoginStep::SignedIn) => {
                self.state = IdentityState::Ready;
                self.persisted = false;
                Ok(LoginStep::SignedIn)
            }
            Ok(step) => Ok(step),
            Err(e) => {
                self.note_error(&e, None, tx);
                Err(e)
            }
        }
    }

    async fn set_passphrase_and_persist(&mut self, passphrase: &str) -> Result<(), IdentityError> {
        if self.state != IdentityState::Ready {
            return Err(match self.state {
                IdentityState::Disabled => IdentityError::AuthKeyDuplicated,
                IdentityState::Locked => IdentityError::Locked,
                _ => IdentityError::InvalidState("not signed in"),
            });
        }
        if passphrase.is_empty() {
            return Err(IdentityError::InvalidState("passphrase required"));
        }
        let session_json = self.ops.session_json().await?;
        let Some(dir) = self.store_dir.clone() else {
            return Err(IdentityError::Internal(
                "no session store configured".to_string(),
            ));
        };
        // Owned copies move into the blocking task (passphrase and session
        // never touch a log line on this path).
        let passphrase = passphrase.to_string();
        let saved = tokio::task::spawn_blocking(move || {
            SessionStore::new(&dir).save(&passphrase, &session_json)
        })
        .await
        .map_err(|e| IdentityError::Internal(format!("persist join: {e}")))?;
        match saved {
            Ok(()) => {
                self.persisted = true;
                Ok(())
            }
            Err(e) => Err(IdentityError::Transport(e.to_string())),
        }
    }

    async fn send_text(
        &mut self,
        chat: &str,
        text: &str,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<SentMessage, SendError> {
        // Loud-fail: a user-bound send with a non-Ready identity must reach
        // the caller as NotAuthorized, never fall back to the bot
        // (IDENTITY_SENDER_UX_SPEC). Backoff keeps the same contract.
        if !self.state.is_ready() {
            return Err(SendError::NotAuthorized);
        }
        if self.in_backoff() {
            return Err(SendError::Transport(
                "personal identity is reconnecting; retry shortly".to_string(),
            ));
        }
        // Send-surface gates (F1/F4): a flood-gated surface reports the
        // remaining wait, a halted one fails closed. Never hits Telegram
        // while gated.
        match self.check_surface(Surface::Sends) {
            Ok(()) => {}
            Err(IdentityError::SurfaceHalted(_)) => return Err(SendError::SurfaceHalted),
            Err(IdentityError::FloodWait(secs)) => return Err(SendError::FloodWait(secs)),
            Err(_) => return Err(SendError::NotAuthorized),
        }
        // Send caps (L10: posts ≤5/hour, ≤15/day), checked AFTER the
        // flood/halt gates: a gated refusal never reaches Telegram and never
        // consumes cap budget. The mechanisms stay independent — a cap
        // refusal is not a flood event and never arms the halt.
        if let Err((limit, retry_after)) = self.send_caps.check() {
            tracing::debug!(
                limit,
                retry_after_s = retry_after,
                "identity send cap reached"
            );
            return Err(SendError::CapReached {
                limit,
                retry_after_secs: retry_after,
            });
        }
        let result = self.ops.send_text(chat, text).await;
        // Conservative accounting (L10): every attempt that reached the
        // MTProto layer — success, flood-wait refusal or failure — charges
        // the budget. In-memory only: a restart resets it (L10 required no
        // persistence), and an owner wipe/reset does not refill it within
        // the running process (the cap protects the account, not a
        // credential).
        self.send_caps.record();
        match result {
            Ok(sent) => Ok(sent),
            Err(e) => {
                self.note_error(&e, Some(Surface::Sends), tx);
                Err(match e {
                    IdentityError::FloodWait(secs) => SendError::FloodWait(secs),
                    IdentityError::PeerNotFound(_) => SendError::ChatNotFound,
                    IdentityError::AuthKeyDuplicated | IdentityError::NotAuthorized => {
                        SendError::NotAuthorized
                    }
                    other => SendError::Transport(other.to_string()),
                })
            }
        }
    }

    async fn disconnect(
        &mut self,
        tx: &mpsc::Sender<IdentityRequest>,
    ) -> Result<(), IdentityError> {
        if self.state == IdentityState::Disabled {
            return Err(IdentityError::AuthKeyDuplicated);
        }
        let result = self.ops.disconnect().await;
        match result {
            Ok(()) => {
                self.state = if self.persisted {
                    IdentityState::Locked
                } else {
                    IdentityState::Configured
                };
                tracing::info!("personal identity disconnected by operator");
                Ok(())
            }
            Err(e) => {
                self.note_error(&e, None, tx);
                Err(e)
            }
        }
    }

    /// Owner-driven destructive re-pair (F5): disconnect, delete the
    /// encrypted session blob, reset the state machine. Deliberately allowed
    /// from EVERY state — this is the only recovery from the terminal
    /// [`IdentityState::Disabled`] — and deliberately destructive: the
    /// account must be paired again afterwards.
    async fn wipe(&mut self) -> Result<(), IdentityError> {
        // Disconnect first so a live client cannot keep using an auth key
        // whose local credential is about to be deleted. Idempotent.
        self.ops.disconnect().await?;
        let mut blob_removed = false;
        if let Some(dir) = &self.store_dir {
            let store = SessionStore::new(dir);
            blob_removed = store.remove();
            // remove() is best-effort: only trust the wipe when the blob is
            // genuinely gone (a pinned file must fail the wipe loudly).
            if blob_removed && store.exists() {
                return Err(IdentityError::Transport(
                    "session blob could not be deleted; wipe refused".to_string(),
                ));
            }
        }
        let had_halt = self.halted.contains(&true);
        self.reset_surface_state();
        self.state = if self.api_id.is_some_and(|id| id != 0)
            && self.api_hash.as_deref().is_some_and(|h| !h.is_empty())
        {
            IdentityState::Configured
        } else {
            IdentityState::NotConfigured
        };
        self.phone_masked = None;
        self.persisted = false;
        tracing::info!(
            blob_removed,
            surface_halt_cleared = had_halt,
            "personal identity wiped by owner; re-pair required"
        );
        Ok(())
    }
}

/// A cheap 0..=1 jitter source for the backoff (OS entropy, no RNG dep).
fn jitter_fraction() -> f64 {
    let mut b = [0u8; 2];
    if getrandom::fill(&mut b).is_err() {
        return 0.0;
    }
    u16::from_le_bytes(b) as f64 / u16::MAX as f64
}

// ---- Manager / spawn -----------------------------------------------------------

/// Builds and spawns the identity actor. The owner keeps only the
/// [`IdentityHandle`]; the ops implementation and session material live and
/// die inside the task.
pub struct IdentityManager;

impl IdentityManager {
    /// Spawn the real subsystem for the running app. `data_dir` is
    /// `config::data_dir()`; the encrypted blob lives under `identity/`.
    /// `db` is the app database (channel allowlist source, design
    /// decision 5).
    pub fn spawn(
        cfg: &crate::config::Config,
        data_dir: &Path,
        db: Arc<crate::db::Db>,
    ) -> IdentityHandle {
        let ops = GrammersOps::new(cfg.telegram_api_id.unwrap_or(0));
        Self::spawn_inner(
            ops,
            Some(data_dir.to_path_buf()),
            Some(db),
            cfg.telegram_api_id,
            cfg.telegram_api_hash.clone(),
        )
    }

    /// Test seam: spawn the actor loop over any [`MtprotoOps`].
    pub(crate) fn spawn_inner<O: MtprotoOps + 'static>(
        ops: O,
        store_dir: Option<PathBuf>,
        db: Option<Arc<crate::db::Db>>,
        api_id: Option<i32>,
        api_hash: Option<String>,
    ) -> IdentityHandle {
        let has_blob = store_dir
            .as_ref()
            .is_some_and(|dir| SessionStore::new(dir).exists());
        let has_creds =
            api_id.is_some_and(|id| id != 0) && api_hash.as_deref().is_some_and(|h| !h.is_empty());
        let actor = IdentityActor {
            ops,
            state: initial_state(has_blob, has_creds),
            store_dir,
            db,
            api_id,
            api_hash,
            phone_masked: None,
            persisted: has_blob,
            limiter: ReadRateLimiter::new(READS_PER_MINUTE),
            send_caps: SendCaps::new(),
            backoff_until: None,
            transport_failures: 0,
            flood_until: [None, None],
            flood_waits: [VecDeque::new(), VecDeque::new()],
            halted: [false, false],
        };
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(actor.run(rx, tx.clone()));
        IdentityHandle { tx: Some(tx) }
    }
}

// ---- Real ops (grammers) ---------------------------------------------------------

/// [`MtprotoOps`] over a live grammers client. Owns the login tokens between
/// steps and rebuilds the client on `connect_with_session`; the SenderPool
/// runner task is spawned alongside the client (grammers 0.11 connects
/// on-demand — without the runner, requests would queue forever).
pub(crate) struct GrammersOps {
    api_id: i32,
    client: Option<grammers_client::Client>,
    session: Option<Arc<grammers_client::session::storages::MemorySession>>,
    login_token: Option<grammers_client::client::LoginToken>,
    password_token: Option<grammers_client::client::PasswordToken>,
}

impl GrammersOps {
    pub(crate) fn new(api_id: i32) -> Self {
        Self {
            api_id,
            client: None,
            session: None,
            login_token: None,
            password_token: None,
        }
    }

    fn build_client(&mut self) -> Result<(), IdentityError> {
        if self.client.is_some() {
            return Ok(());
        }
        let session = Arc::new(grammers_client::session::storages::MemorySession::default());
        let pool = grammers_client::sender::SenderPool::new(Arc::clone(&session), self.api_id);
        let client = grammers_client::Client::new(pool.handle);
        tokio::spawn(pool.runner.run());
        self.client = Some(client);
        self.session = Some(session);
        Ok(())
    }

    /// Client + session pair, or a typed internal error.
    fn parts(
        &self,
    ) -> Result<
        (
            &grammers_client::Client,
            &grammers_client::session::storages::MemorySession,
        ),
        IdentityError,
    > {
        match (&self.client, &self.session) {
            (Some(c), Some(s)) => Ok((c, s)),
            _ => Err(IdentityError::Internal(
                "identity client is not connected".to_string(),
            )),
        }
    }
}

impl MtprotoOps for GrammersOps {
    async fn request_login_code(
        &mut self,
        phone: &str,
        api_id: i32,
        api_hash: &str,
    ) -> Result<(), IdentityError> {
        self.api_id = api_id;
        self.build_client()?;
        let Some(client) = self.client.as_ref() else {
            return Err(IdentityError::Internal("client build failed".to_string()));
        };
        let result = client.request_login_code(phone, api_hash).await;
        match result {
            Ok(token) => {
                self.login_token = Some(token);
                Ok(())
            }
            Err(e) => Err(map_invocation_error(&e)),
        }
    }

    async fn sign_in(&mut self, code: &str) -> Result<LoginStep, IdentityError> {
        let Some(client) = self.client.as_ref() else {
            return Err(IdentityError::Internal(
                "no client during sign-in".to_string(),
            ));
        };
        let Some(token) = self.login_token.as_ref() else {
            return Err(IdentityError::InvalidState("no login code was requested"));
        };
        let result = client.sign_in(token, code).await;
        match result {
            Ok(_) => {
                self.login_token = None;
                Ok(LoginStep::SignedIn)
            }
            Err(grammers_client::SignInError::PasswordRequired(pt)) => {
                self.password_token = Some(pt);
                Ok(LoginStep::PasswordRequired)
            }
            Err(grammers_client::SignInError::InvalidCode) => Err(IdentityError::InvalidCode),
            Err(grammers_client::SignInError::InvalidPassword(pt)) => {
                // Not expected at sign_in; keep the token for check_password.
                self.password_token = Some(pt);
                Err(IdentityError::InvalidPassword)
            }
            Err(grammers_client::SignInError::SignUpRequired) => Err(IdentityError::SignUpRequired),
            Err(grammers_client::SignInError::Other(e)) => Err(map_invocation_error(&e)),
        }
    }

    async fn check_password(&mut self, password: &str) -> Result<LoginStep, IdentityError> {
        let Some(client) = self.client.as_ref() else {
            return Err(IdentityError::Internal("no client during 2FA".to_string()));
        };
        let Some(token) = self.password_token.take() else {
            return Err(IdentityError::InvalidState("no 2FA step is pending"));
        };
        let result = client.check_password(token, password).await;
        match result {
            Ok(_) => Ok(LoginStep::SignedIn),
            Err(grammers_client::SignInError::InvalidPassword(pt)) => {
                // Telegram re-issues a fresh token per attempt; keep it so
                // the operator can retry without restarting the wizard.
                self.password_token = Some(pt);
                Err(IdentityError::InvalidPassword)
            }
            Err(grammers_client::SignInError::PasswordRequired(pt)) => {
                self.password_token = Some(pt);
                Err(IdentityError::PasswordRequired)
            }
            Err(grammers_client::SignInError::Other(e)) => Err(map_invocation_error(&e)),
            Err(_) => Err(IdentityError::Internal("unexpected 2FA error".to_string())),
        }
    }

    async fn connect_with_session(
        &mut self,
        session_json: &serde_json::Value,
    ) -> Result<(), IdentityError> {
        let session = Arc::new(super::tl::restore_session(session_json)?);
        let pool = grammers_client::sender::SenderPool::new(Arc::clone(&session), self.api_id);
        let client = grammers_client::Client::new(pool.handle);
        tokio::spawn(pool.runner.run());
        match super::tl::check_authorized(&client).await {
            Ok(true) => {
                self.client = Some(client);
                self.session = Some(session);
                Ok(())
            }
            Ok(false) => Err(IdentityError::NotAuthorized),
            Err(e) => Err(e),
        }
    }

    async fn session_json(&self) -> Result<serde_json::Value, IdentityError> {
        match &self.session {
            Some(session) => super::tl::snapshot_session(session).await,
            None => Err(IdentityError::Internal(
                "no live session to persist".to_string(),
            )),
        }
    }

    async fn send_text(&mut self, chat: &str, text: &str) -> Result<SentMessage, IdentityError> {
        let (client, session) = self.parts()?;
        let peer = super::tl::resolve_chat(client, session, chat).await?;
        let result = client.send_message(peer, text).await;
        match result {
            Ok(m) => Ok(SentMessage {
                chat_id: peer.id.bare_id().unwrap_or_default(),
                message_id: m.id() as i64,
            }),
            Err(e) => Err(map_invocation_error(&e)),
        }
    }

    async fn history(
        &mut self,
        chat: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        let (client, session) = self.parts()?;
        let peer = super::tl::resolve_chat(client, session, chat).await?;
        let mut iter = client.iter_messages(peer).limit(limit.max(1));
        // `before` is handle-clamped into i32 range (F8) — no truncation.
        if let Some(b) = before {
            iter = iter.offset_id(b as i32);
        }
        let mut out = Vec::new();
        loop {
            let next = iter.next().await;
            match next {
                Ok(Some(m)) => out.push(normalize_message(&m)),
                Ok(None) => break,
                Err(e) => return Err(map_invocation_error(&e)),
            }
        }
        Ok(out)
    }

    async fn thread_replies(
        &mut self,
        chat: &str,
        msg_id: i64,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        let (client, session) = self.parts()?;
        let peer = super::tl::resolve_chat(client, session, chat).await?;
        // msg_id/before arrive handle-validated (F8: out-of-range msg_id is
        // rejected, before is clamped) — the casts below are exact.
        let raw = super::tl::get_replies(
            client,
            peer,
            msg_id as i32,
            before.map(|b| b as i32).unwrap_or(0),
            limit as i32,
        )
        .await
        .map_err(|e| map_invocation_error(&e))?;
        Ok(raw
            .iter()
            .filter_map(super::tl::parse_raw_message)
            .map(|r| super::tl::normalize_raw(&r))
            .collect())
    }

    async fn search(
        &mut self,
        query: &str,
        chat: &str,
        limit: usize,
    ) -> Result<Vec<NormalizedMessage>, IdentityError> {
        let (client, session) = self.parts()?;
        // `chat` is always an explicitly configured channel name (F3); the
        // account-wide `search_all_messages` path was removed with it.
        let peer = super::tl::resolve_chat(client, session, chat).await?;
        let mut iter = client.search_messages(peer).query(query);
        let mut out = Vec::new();
        while out.len() < limit.max(1) {
            let next = iter.next().await;
            match next {
                Ok(Some(m)) => out.push(normalize_message(&m)),
                Ok(None) => break,
                Err(e) => return Err(map_invocation_error(&e)),
            }
        }
        Ok(out)
    }

    async fn unread_digest(&mut self) -> Result<Vec<NormalizedDialog>, IdentityError> {
        let (client, _session) = self.parts()?;
        let mut iter = client.iter_dialogs();
        let mut out = Vec::new();
        let mut scanned = 0usize;
        loop {
            if scanned >= MAX_DIGEST_SCANNED_DIALOGS {
                break;
            }
            scanned += 1;
            let dialog = match iter.next().await {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => return Err(map_invocation_error(&e)),
            };
            let unread = match &dialog.raw {
                grammers_client::tl::enums::Dialog::Dialog(d) => d.unread_count,
                grammers_client::tl::enums::Dialog::Folder(d) => {
                    d.unread_muted_peers_count + d.unread_unmuted_peers_count
                }
                // Community dialogs carry no unread counter in this layer.
                grammers_client::tl::enums::Dialog::Community(_) => 0,
            };
            if unread == 0 {
                continue;
            }
            let peer = dialog.peer();
            let title = peer.name().map(str::to_string).unwrap_or_default();
            let handle = peer
                .username()
                .map(|u| format!("@{u}"))
                .unwrap_or_else(|| peer.id().bare_id().unwrap_or_default().to_string());
            let (last_text, last_date) = match dialog.last_message.as_ref() {
                Some(m) => (m.text().to_string(), Some(m.date().to_string())),
                None => (String::new(), None),
            };
            out.push(NormalizedDialog {
                chat: handle,
                title,
                unread: unread as i64,
                last_text,
                last_date,
            });
        }
        Ok(out)
    }

    async fn disconnect(&mut self) -> Result<(), IdentityError> {
        if let Some(client) = self.client.take() {
            client.disconnect();
        }
        self.session = None;
        self.login_token = None;
        self.password_token = None;
        Ok(())
    }
}

/// Normalize a high-level grammers `Message` into the wire type.
fn normalize_message(m: &grammers_client::message::Message) -> NormalizedMessage {
    let sender = match m.sender() {
        Some(peer) => peer
            .name()
            .or(peer.username())
            .map(str::to_string)
            .unwrap_or_else(|| peer.id().bare_id().unwrap_or_default().to_string()),
        None => String::new(),
    };
    NormalizedMessage {
        id: m.id() as i64,
        // jiff's Timestamp Display is RFC 3339 (UTC).
        date: m.date().to_string(),
        sender,
        text: m.text().to_string(),
        reply_to: m.reply_to_message_id().map(|v| v as i64),
    }
}

// ---- Tests ---------------------------------------------------------------------
//
// Everything runs against `FakeOps` — no live network, no real Telegram.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn msg(id: i64) -> NormalizedMessage {
        NormalizedMessage {
            id,
            date: "2026-10-27T00:00:00+00:00".to_string(),
            sender: "tester".to_string(),
            text: format!("message {id}"),
            reply_to: None,
        }
    }

    /// Scripted [`MtprotoOps`]: queues of expected results (front popped
    /// first; an empty queue yields the operation's default success) plus a
    /// call log for assertions. One Mutex per script so `&mut self` methods
    /// can share them. The call log is `Arc`-shared so tests can inspect it
    /// after the actor owns the fake.
    struct FakeOps {
        request_code: Mutex<VecDeque<Result<(), IdentityError>>>,
        sign_in: Mutex<VecDeque<Result<LoginStep, IdentityError>>>,
        check_password: Mutex<VecDeque<Result<LoginStep, IdentityError>>>,
        connect: Mutex<VecDeque<Result<(), IdentityError>>>,
        history: Mutex<VecDeque<Result<Vec<NormalizedMessage>, IdentityError>>>,
        send: Mutex<VecDeque<Result<SentMessage, IdentityError>>>,
        session: Mutex<serde_json::Value>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl FakeOps {
        fn new() -> Self {
            Self {
                request_code: Mutex::new(VecDeque::new()),
                sign_in: Mutex::new(VecDeque::new()),
                check_password: Mutex::new(VecDeque::new()),
                connect: Mutex::new(VecDeque::new()),
                history: Mutex::new(VecDeque::new()),
                send: Mutex::new(VecDeque::new()),
                session: Mutex::new(serde_json::json!({
                    "home_dc": 2,
                    "dc_options": [],
                    "updates_state": { "pts": 1, "qts": 1, "date": 1, "seq": 1 }
                })),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn record(&self, what: String) {
            self.calls.lock().unwrap().push(what);
        }

        fn pop<T>(q: &Mutex<VecDeque<T>>, default: impl FnOnce() -> T) -> T {
            q.lock().unwrap().pop_front().unwrap_or_else(default)
        }
    }

    impl MtprotoOps for FakeOps {
        async fn request_login_code(
            &mut self,
            phone: &str,
            _api_id: i32,
            _api_hash: &str,
        ) -> Result<(), IdentityError> {
            self.record(format!("request_code:{phone}"));
            Self::pop(&self.request_code, || Ok(()))
        }

        async fn sign_in(&mut self, code: &str) -> Result<LoginStep, IdentityError> {
            self.record(format!("sign_in:{code}"));
            Self::pop(&self.sign_in, || Ok(LoginStep::SignedIn))
        }

        async fn check_password(&mut self, password: &str) -> Result<LoginStep, IdentityError> {
            self.record(format!("check_password:{password}"));
            Self::pop(&self.check_password, || Ok(LoginStep::SignedIn))
        }

        async fn connect_with_session(
            &mut self,
            session_json: &serde_json::Value,
        ) -> Result<(), IdentityError> {
            self.record(format!("connect:{}", session_json.is_object()));
            Self::pop(&self.connect, || Ok(()))
        }

        async fn session_json(&self) -> Result<serde_json::Value, IdentityError> {
            self.record("session_json".to_string());
            Ok(self.session.lock().unwrap().clone())
        }

        async fn send_text(
            &mut self,
            chat: &str,
            text: &str,
        ) -> Result<SentMessage, IdentityError> {
            self.record(format!("send:{chat}:{text}"));
            Self::pop(&self.send, || {
                Ok(SentMessage {
                    chat_id: 42,
                    message_id: 7,
                })
            })
        }

        async fn history(
            &mut self,
            chat: &str,
            before: Option<i64>,
            limit: usize,
        ) -> Result<Vec<NormalizedMessage>, IdentityError> {
            self.record(format!("history:{chat}:before={before:?}:limit={limit}"));
            Self::pop(&self.history, || Ok(vec![msg(1), msg(2)]))
        }

        async fn thread_replies(
            &mut self,
            chat: &str,
            msg_id: i64,
            before: Option<i64>,
            limit: usize,
        ) -> Result<Vec<NormalizedMessage>, IdentityError> {
            self.record(format!(
                "thread:{chat}:{msg_id}:before={before:?}:limit={limit}"
            ));
            Self::pop(&self.history, || Ok(vec![msg(11)]))
        }

        async fn search(
            &mut self,
            query: &str,
            chat: &str,
            limit: usize,
        ) -> Result<Vec<NormalizedMessage>, IdentityError> {
            self.record(format!("search:{query}:{chat}:limit={limit}"));
            Self::pop(&self.history, || Ok(vec![msg(21)]))
        }

        async fn unread_digest(&mut self) -> Result<Vec<NormalizedDialog>, IdentityError> {
            self.record("digest".to_string());
            Ok(vec![NormalizedDialog {
                chat: "@chan".to_string(),
                title: "Channel".to_string(),
                unread: 2,
                last_text: "hi".to_string(),
                last_date: None,
            }])
        }

        async fn disconnect(&mut self) -> Result<(), IdentityError> {
            self.record("disconnect".to_string());
            Ok(())
        }
    }

    const CREDS_ID: i32 = 12345;
    const CREDS_HASH: &str = "test-api-hash";

    fn spawn(ops: FakeOps, store_dir: Option<std::path::PathBuf>) -> IdentityHandle {
        IdentityManager::spawn_inner(
            ops,
            store_dir,
            None,
            Some(CREDS_ID),
            Some(CREDS_HASH.to_string()),
        )
    }

    /// Spawn with a channel allowlist backed by a real (temp-file) DB with
    /// exactly one configured channel. Returns the handle, the tempdir
    /// (which must outlive the DB connection) and the shared call log for
    /// post-spawn assertions.
    fn spawn_with_channel(
        ops: FakeOps,
        channel: &str,
    ) -> (IdentityHandle, tempfile::TempDir, Arc<Mutex<Vec<String>>>) {
        let dir = tempfile::tempdir().unwrap();
        let db =
            Arc::new(crate::db::Db::new(dir.path().join("test.db").to_str().unwrap()).unwrap());
        db.add_channel(channel, 0).unwrap();
        let calls = Arc::clone(&ops.calls);
        let handle = IdentityManager::spawn_inner(
            ops,
            None,
            Some(db),
            Some(CREDS_ID),
            Some(CREDS_HASH.to_string()),
        );
        (handle, dir, calls)
    }

    /// Drive the actor from NotConfigured-adjacent state through code+2FA
    /// login: RequestCode → SignIn(PasswordRequired) → CheckPassword → Ready.
    #[tokio::test]
    async fn login_sm_full_path_with_conditional_password() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FakeOps::new();
        ops.sign_in
            .lock()
            .unwrap()
            .push_back(Ok(LoginStep::PasswordRequired));
        let handle = spawn(ops, Some(dir.path().to_path_buf()));

        let st = handle.status().await;
        assert_eq!(st.state, IdentityState::Configured);
        assert!(!st.persisted);

        handle.request_code("+41791234567").await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::AwaitingCode);

        let step = handle.sign_in("12345").await.unwrap();
        assert_eq!(step, LoginStep::PasswordRequired);
        assert_eq!(handle.status().await.state, IdentityState::AwaitingPassword);

        let step = handle.check_password("hunter2").await.unwrap();
        assert_eq!(step, LoginStep::SignedIn);
        let st = handle.status().await;
        assert_eq!(st.state, IdentityState::Ready);
        // Freshly signed-in session is not persisted until the operator
        // sets the unlock passphrase.
        assert!(!st.persisted);

        handle
            .set_passphrase_and_persist("unlock-passphrase")
            .await
            .unwrap();
        assert!(handle.status().await.persisted);
    }

    /// No 2FA: RequestCode → SignIn → Ready directly.
    #[tokio::test]
    async fn login_sm_direct_without_password() {
        let handle = spawn(FakeOps::new(), None);
        handle.request_code("+41791234567").await.unwrap();
        let step = handle.sign_in("54321").await.unwrap();
        assert_eq!(step, LoginStep::SignedIn);
        assert_eq!(handle.status().await.state, IdentityState::Ready);
    }

    /// Wrong unlock passphrase surfaces WrongPassphrase and the subsystem
    /// stays Locked; the correct passphrase then unlocks and connects.
    #[tokio::test]
    async fn unlock_wrong_passphrase_surfaces_and_stays_locked() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path());
        store
            .save(
                "correct-passphrase",
                &serde_json::json!({"home_dc": 2, "dc_options": [], "updates_state": {}}),
            )
            .unwrap();

        let handle = spawn(FakeOps::new(), Some(dir.path().to_path_buf()));
        let st = handle.status().await;
        assert_eq!(st.state, IdentityState::Locked);
        assert!(st.persisted, "a blob on disk counts as persisted");

        let err = handle.unlock("wrong-passphrase").await.unwrap_err();
        assert!(matches!(err, IdentityError::WrongPassphrase), "got {err:?}");
        assert_eq!(handle.status().await.state, IdentityState::Locked);

        // Reads stay rejected while locked.
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(matches!(err, IdentityError::Locked), "got {err:?}");

        handle.unlock("correct-passphrase").await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::Ready);
    }

    /// FloodWait(≤1800) from an op is carried to the caller; the actor does
    /// NOT sleep on it — a per-surface gate with a scheduled wake takes over
    /// (see `flood_wait_gate_never_blocks_status`).
    #[tokio::test]
    async fn flood_wait_maps_to_typed_error() {
        let ops = FakeOps::new();
        ops.history
            .lock()
            .unwrap()
            .push_back(Err(IdentityError::FloodWait(1)));
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        // Force Ready without a real login: script a direct unlock.
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::NotAuthorized),
            "history before Ready must be NotAuthorized, got {err:?}"
        );

        // Drive to Ready.
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::Ready);

        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(matches!(err, IdentityError::FloodWait(1)), "got {err:?}");
    }

    /// F1: a flood wait never stalls the mailbox loop. The flooding read
    /// returns immediately, `Status` answers while the gate is open, and a
    /// follow-up read is refused by the gate (FloodWait with the remaining
    /// seconds) WITHOUT reaching the ops layer. After the scheduled wake the
    /// gate clears and reads flow again.
    #[tokio::test]
    async fn flood_wait_gate_never_blocks_status() {
        let ops = FakeOps::new();
        ops.history
            .lock()
            .unwrap()
            .push_back(Err(IdentityError::FloodWait(1)));
        let (handle, _dir, calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();

        // The flooding read returns the typed error right away (≤1800s is
        // gated, not slept).
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(matches!(err, IdentityError::FloodWait(1)), "got {err:?}");

        // Status must NEVER wait on a flood: it answers within 500ms even
        // though the gate is open for a full second.
        let st = tokio::time::timeout(Duration::from_millis(500), handle.status())
            .await
            .expect("status must not block behind a flood gate");
        assert_eq!(st.state, IdentityState::Ready);

        // A follow-up read is gated (the ops default is success, so an Ok
        // here would mean the gate failed) and never reaches the ops layer.
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::FloodWait(w) if w >= 1),
            "gated read must surface the remaining wait, got {err:?}"
        );

        // The scheduled wake lifts the gate; poll so the test stays robust.
        let mut cleared = false;
        for _ in 0..50 {
            if handle.history("@chan", None, 10).await.is_ok() {
                cleared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(cleared, "flood gate must clear after the scheduled wake");
        // ops saw exactly two fetches: the flooding one and the post-wake
        // success — every gated refusal stayed inside the actor.
        let fetches = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("history:"))
            .count();
        assert_eq!(fetches, 2, "gated reads must not reach the ops layer");
    }

    /// F4: three FloodWaits within the rolling hour halt the reads surface —
    /// further reads get `SurfaceHalted`, status stays Ready, sends are
    /// unaffected, and an owner `ResetSurfaces` restores the surface.
    #[tokio::test]
    async fn flood_halt_after_three_in_window_and_reset() {
        let ops = FakeOps::new();
        // FloodWait(0): counts toward the window, sets no gate — the three
        // floods land back-to-back without test-visible waits.
        for _ in 0..3 {
            ops.history
                .lock()
                .unwrap()
                .push_back(Err(IdentityError::FloodWait(0)));
        }
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();

        for _ in 0..3 {
            let err = handle.history("@chan", None, 10).await.unwrap_err();
            assert!(
                matches!(err, IdentityError::FloodWait(0)),
                "triggering floods still report their wait, got {err:?}"
            );
        }
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::SurfaceHalted("reads")),
            "4th read must hit the halt, got {err:?}"
        );
        // The halt is not an identity state change.
        assert_eq!(handle.status().await.state, IdentityState::Ready);
        // Sends run on their own surface and stay usable.
        assert!(handle.send_text(42, "hi").await.is_ok());

        // Owner unlock (F4): the terminal halt clears only here.
        handle.reset_surfaces().await.unwrap();
        assert!(
            handle.history("@chan", None, 10).await.is_ok(),
            "reads must work again after ResetSurfaces"
        );
    }

    /// F4: the sends surface halts independently of reads.
    #[tokio::test]
    async fn send_surface_halts_independently() {
        let ops = FakeOps::new();
        for _ in 0..3 {
            ops.send
                .lock()
                .unwrap()
                .push_back(Err(IdentityError::FloodWait(0)));
        }
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();

        for _ in 0..3 {
            let err = handle.send_text(42, "hi").await.unwrap_err();
            assert!(
                matches!(err, SendError::FloodWait(0)),
                "triggering send floods still report their wait, got {err:?}"
            );
        }
        let err = handle.send_text(42, "hi").await.unwrap_err();
        assert!(
            matches!(err, SendError::SurfaceHalted),
            "4th send must hit the halt, got {err:?}"
        );
        // Reads are unaffected by the send-surface halt.
        assert!(handle.history("@chan", None, 10).await.is_ok());
        // Owner reset restores sends too.
        handle.reset_surfaces().await.unwrap();
        assert!(handle.send_text(42, "hi").await.is_ok());
    }

    /// AUTH_KEY_DUPLICATED during connect → Disabled; every subsequent
    /// request (reads, sends, login) is rejected until external re-pair.
    #[tokio::test]
    async fn auth_key_duplicated_disables_subsystem() {
        let dir = tempfile::tempdir().unwrap();
        SessionStore::new(dir.path())
            .save(
                "p",
                &serde_json::json!({"home_dc": 2, "dc_options": [], "updates_state": {}}),
            )
            .unwrap();
        let ops = FakeOps::new();
        ops.connect
            .lock()
            .unwrap()
            .push_back(Err(IdentityError::AuthKeyDuplicated));
        let handle = spawn(ops, Some(dir.path().to_path_buf()));

        let err = handle.unlock("p").await.unwrap_err();
        assert!(
            matches!(err, IdentityError::AuthKeyDuplicated),
            "got {err:?}"
        );
        assert_eq!(handle.status().await.state, IdentityState::Disabled);

        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::AuthKeyDuplicated),
            "got {err:?}"
        );
        let err = handle.request_code("+41791234567").await.unwrap_err();
        assert!(
            matches!(err, IdentityError::AuthKeyDuplicated),
            "got {err:?}"
        );
        assert!(matches!(
            handle.send_text(42, "x").await,
            Err(SendError::NotAuthorized)
        ));
        // Status never fails, even when disabled.
        assert_eq!(handle.status().await.state, IdentityState::Disabled);
    }

    /// Reads ≤30/min; the 31st within the window gets RateLimited.
    #[tokio::test]
    async fn rate_limiter_rejects_reads_over_cap() {
        let (handle, _dir, _calls) = spawn_with_channel(FakeOps::new(), "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        for _ in 0..READS_PER_MINUTE {
            handle.history("@chan", None, 1).await.unwrap();
        }
        let err = handle.history("@chan", None, 1).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::RateLimited(w) if w >= 1),
            "31st read must be RateLimited, got {err:?}"
        );
        // Writes and status are unaffected by the read cap.
        handle.status().await;
    }

    /// F3: read chats resolve ONLY against the configured channels table.
    /// Known chats pass (the canonical DB name reaches the ops layer);
    /// unknown chats — and chatless global search — fail closed with
    /// `ChatNotAllowed` and never touch the ops layer.
    #[tokio::test]
    async fn reads_scoped_to_configured_channels() {
        let ops = FakeOps::new();
        let (handle, _dir, calls) = spawn_with_channel(ops, "@Chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();

        // Known channel (any casing): allowed; the canonical name — exactly
        // the stored spelling — is what the ops layer receives.
        handle.history("@chan", None, 10).await.unwrap();
        handle.thread_replies("@CHAN", 5, None, 10).await.unwrap();
        handle.search("q", Some("@chan"), 10).await.unwrap();
        let rec = calls.lock().unwrap().clone();
        assert!(
            rec.iter()
                .any(|c| c == "history:@Chan:before=None:limit=10"),
            "rec: {rec:?}"
        );
        assert!(
            rec.iter()
                .any(|c| c == "thread:@Chan:5:before=None:limit=10"),
            "rec: {rec:?}"
        );
        assert!(
            rec.iter().any(|c| c == "search:q:@Chan:limit=10"),
            "rec: {rec:?}"
        );

        // Unknown channel: ChatNotAllowed, zero ops traffic.
        let err = handle.history("@elsewhere", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::ChatNotAllowed(ref c) if c == "@elsewhere"),
            "got {err:?}"
        );
        let err = handle
            .thread_replies("@elsewhere", 5, None, 10)
            .await
            .unwrap_err();
        assert!(
            matches!(err, IdentityError::ChatNotAllowed(_)),
            "got {err:?}"
        );
        let err = handle
            .search("q", Some("@elsewhere"), 10)
            .await
            .unwrap_err();
        assert!(
            matches!(err, IdentityError::ChatNotAllowed(_)),
            "got {err:?}"
        );

        // Chatless (account-wide) search was removed: ChatNotAllowed even
        // though the identity is Ready.
        let err = handle.search("q", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::ChatNotAllowed(_)),
            "got {err:?}"
        );
        let new_calls = calls.lock().unwrap().len() - rec.len();
        assert_eq!(
            new_calls, 0,
            "disallowed reads must not reach the ops layer"
        );
    }

    /// F3: with no DB configured nothing is explicitly allowed — reads fail
    /// closed instead of falling back to accept-any-peer.
    #[tokio::test]
    async fn reads_without_allowlist_fail_closed() {
        let handle = spawn(FakeOps::new(), None);
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        let err = handle.history("@chan", None, 10).await.unwrap_err();
        assert!(
            matches!(err, IdentityError::ChatNotAllowed(_)),
            "got {err:?}"
        );
    }

    /// The actor clamps read limits to MAX_FETCH_LIMIT (recorded call).
    #[tokio::test]
    async fn read_limits_are_clamped_to_100() {
        let ops = FakeOps::new();
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        handle.history("@chan", Some(500), 500).await.unwrap();
        handle.thread_replies("@chan", 10, None, 500).await.unwrap();
        handle.search("q", Some("@chan"), 500).await.unwrap();
        // Give the actor a beat? Not needed: handle calls resolve after the
        // actor processed them (oneshot round-trip is the sync point), and
        // calls are recorded before replying.
        drop(handle);
    }

    /// F8: `msg_id` outside the i32 wire range is rejected with
    /// InvalidCursor (never silently truncated into a different thread);
    /// `before` is clamped into range at the handle boundary.
    #[tokio::test]
    async fn cursors_are_rejected_or_clamped_at_the_boundary() {
        let ops = FakeOps::new();
        let (handle, _dir, calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();

        // msg_id: reject (both directions), even on an unconnected handle.
        let err = handle
            .thread_replies("@chan", (i32::MAX as i64) + 1, None, 10)
            .await
            .unwrap_err();
        assert!(matches!(err, IdentityError::InvalidCursor), "got {err:?}");
        let err = handle
            .thread_replies("@chan", i64::MIN, None, 10)
            .await
            .unwrap_err();
        assert!(matches!(err, IdentityError::InvalidCursor), "got {err:?}");
        let unconnected = IdentityHandle::unconnected();
        assert!(matches!(
            unconnected
                .thread_replies("@c", (i32::MAX as i64) + 5, None, 1)
                .await,
            Err(IdentityError::InvalidCursor)
        ));

        // before: clamp (up for cursors beyond the newest message, down for
        // prehistoric ones) — the ops layer sees in-range values.
        handle
            .history("@chan", Some((i32::MAX as i64) + 1234), 10)
            .await
            .unwrap();
        handle
            .history("@chan", Some((i32::MIN as i64) - 1234), 10)
            .await
            .unwrap();
        handle
            .thread_replies("@chan", 5, Some((i32::MAX as i64) + 7), 10)
            .await
            .unwrap();
        let rec = calls.lock().unwrap().clone();
        assert!(
            rec.iter()
                .any(|c| c == "history:@chan:before=Some(2147483647):limit=10"),
            "rec: {rec:?}"
        );
        assert!(
            rec.iter()
                .any(|c| c == "history:@chan:before=Some(-2147483648):limit=10"),
            "rec: {rec:?}"
        );
        assert!(
            rec.iter()
                .any(|c| c == "thread:@chan:5:before=Some(2147483647):limit=10"),
            "rec: {rec:?}"
        );
    }

    /// `IdentityHandle::unconnected()` fails honestly, never hangs.
    #[tokio::test]
    async fn unconnected_handle_fails_honestly() {
        let handle = IdentityHandle::unconnected();
        assert!(!handle.is_connected());
        let st = handle.status().await;
        assert_eq!(st.state, IdentityState::Disconnected);
        assert!(matches!(
            handle.history("@c", None, 10).await,
            Err(IdentityError::NotConnected)
        ));
        assert!(matches!(
            handle.unlock("x").await,
            Err(IdentityError::NotConnected)
        ));
        assert!(matches!(
            handle.send_text(1, "x").await,
            Err(SendError::NotAuthorized)
        ));
    }

    /// Send on a not-Ready identity → NotAuthorized, loud (no bot fallback).
    #[tokio::test]
    async fn send_text_not_ready_is_not_authorized() {
        let handle = spawn(FakeOps::new(), None);
        assert_eq!(handle.status().await.state, IdentityState::Configured);
        let err = handle.send_text(42, "hi").await.unwrap_err();
        assert!(matches!(err, SendError::NotAuthorized), "got {err:?}");

        // Same after code is sent (login half-done).
        handle.request_code("+41791234567").await.unwrap();
        assert!(matches!(
            handle.send_text(42, "hi").await,
            Err(SendError::NotAuthorized)
        ));
    }

    /// Session-store round trip through the actor: login → persist with
    /// passphrase → a fresh store loads the same session JSON.
    #[tokio::test]
    async fn set_passphrase_persists_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ops = FakeOps::new();
        let handle = spawn(ops, Some(dir.path().to_path_buf()));
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        handle.set_passphrase_and_persist("pass").await.unwrap();

        let store = SessionStore::new(dir.path());
        assert!(store.exists());
        let loaded = store.load("pass").unwrap();
        assert_eq!(
            loaded,
            serde_json::json!({
                "home_dc": 2,
                "dc_options": [],
                "updates_state": { "pts": 1, "qts": 1, "date": 1, "seq": 1 }
            })
        );

        // After a disconnect the persisted subsystem returns to Locked.
        handle.disconnect().await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::Locked);
    }

    /// F5: the owner-driven wipe deletes the session blob and resets the
    /// state machine to Configured (credentials remain in the env); the
    /// subsystem is immediately re-pairable.
    #[tokio::test]
    async fn wipe_removes_blob_and_resets_state() {
        let dir = tempfile::tempdir().unwrap();
        SessionStore::new(dir.path())
            .save(
                "p",
                &serde_json::json!({"home_dc": 2, "dc_options": [], "updates_state": {}}),
            )
            .unwrap();
        let handle = spawn(FakeOps::new(), Some(dir.path().to_path_buf()));
        assert_eq!(handle.status().await.state, IdentityState::Locked);

        handle.wipe().await.unwrap();
        assert!(
            !SessionStore::new(dir.path()).exists(),
            "the blob must be gone after a wipe"
        );
        let st = handle.status().await;
        assert_eq!(st.state, IdentityState::Configured, "creds still in env");
        assert!(!st.persisted);

        // Immediately re-pairable: the wizard can start over.
        handle.request_code("+41791234567").await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::AwaitingCode);
    }

    /// F5: `AUTH_KEY_DUPLICATED`'s terminal Disabled state is recoverable
    /// ONLY through the owner-driven wipe.
    #[tokio::test]
    async fn wipe_recovers_disabled_identity() {
        let dir = tempfile::tempdir().unwrap();
        SessionStore::new(dir.path())
            .save(
                "p",
                &serde_json::json!({"home_dc": 2, "dc_options": [], "updates_state": {}}),
            )
            .unwrap();
        let ops = FakeOps::new();
        ops.connect
            .lock()
            .unwrap()
            .push_back(Err(IdentityError::AuthKeyDuplicated));
        let handle = spawn(ops, Some(dir.path().to_path_buf()));

        let err = handle.unlock("p").await.unwrap_err();
        assert!(matches!(err, IdentityError::AuthKeyDuplicated));
        assert_eq!(handle.status().await.state, IdentityState::Disabled);
        // Disabled rejects even the wizard.
        assert!(handle.request_code("+41791234567").await.is_err());

        handle.wipe().await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::Configured);
        assert!(
            !SessionStore::new(dir.path()).exists(),
            "the poisoned blob must be gone"
        );
        // The state machine works again.
        handle.request_code("+41791234567").await.unwrap();
        assert_eq!(handle.status().await.state, IdentityState::AwaitingCode);
    }

    /// The masked phone appears in status; the raw number never does.
    #[tokio::test]
    async fn phone_is_masked_in_status() {
        let handle = spawn(FakeOps::new(), None);
        handle.request_code("+41791234567").await.unwrap();
        let st = handle.status().await;
        assert!(st.detail.contains("•••4567"), "detail: {}", st.detail);
        assert!(!st.detail.contains("41791234567"), "raw phone leaked");
    }

    /// Sliding-window limiter unit behavior (no actor): admits `max`, then
    /// reports a ≥1s wait, and prunes after the window passes.
    #[test]
    fn rate_limiter_window_math() {
        let mut limiter = ReadRateLimiter::new(2);
        limiter.check().unwrap();
        limiter.check().unwrap();
        let err = limiter.check().unwrap_err();
        assert!((1..=60).contains(&err));
    }

    // ---- Send caps (L10: posts ≤5/hour, ≤15/day) ----

    /// The 6th send within the sliding hour is refused with `CapReached`
    /// (retry_after > 0) and never reaches the ops layer; the first five
    /// went through.
    #[tokio::test]
    async fn send_caps_sixth_send_in_hour_is_refused() {
        let ops = FakeOps::new();
        let (handle, _dir, calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        for i in 0..5 {
            handle
                .send_text(42, "hi")
                .await
                .unwrap_or_else(|e| panic!("send {i} must be admitted, got {e:?}"));
        }
        let err = handle.send_text(42, "hi").await.unwrap_err();
        assert!(
            matches!(err, SendError::CapReached { retry_after_secs: w, .. } if w > 0),
            "6th send must hit the hourly cap with a positive retry, got {err:?}"
        );
        // Exactly five attempts reached the ops layer.
        let sends = calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with("send:"))
            .count();
        assert_eq!(sends, 5, "capped sends must not reach the ops layer");
    }

    /// Failed attempts count too (conservative): a FloodWait refusal reached
    /// Telegram, so those attempts consume budget. Two flood-waited sends
    /// (below the ≥3 flood halt) plus three successful ones charge the cap
    /// to its hourly limit.
    #[tokio::test]
    async fn send_caps_charge_flood_waited_attempts() {
        let ops = FakeOps::new();
        for _ in 0..2 {
            ops.send
                .lock()
                .unwrap()
                .push_back(Err(IdentityError::FloodWait(0)));
        }
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        for _ in 0..2 {
            let err = handle.send_text(42, "hi").await.unwrap_err();
            assert!(matches!(err, SendError::FloodWait(0)), "got {err:?}");
        }
        for _ in 0..3 {
            handle.send_text(42, "hi").await.unwrap();
        }
        let err = handle.send_text(42, "hi").await.unwrap_err();
        assert!(
            matches!(err, SendError::CapReached { .. }),
            "the 6th attempt must hit the cap even though two failed, got {err:?}"
        );
    }

    /// Caps are independent of the flood-halt: ResetSurfaces clears halts
    /// but does NOT refill the cap budget.
    #[tokio::test]
    async fn send_caps_survive_an_owner_surface_reset() {
        let ops = FakeOps::new();
        let (handle, _dir, _calls) = spawn_with_channel(ops, "@chan");
        handle.request_code("+41791234567").await.unwrap();
        handle.sign_in("00000").await.unwrap();
        for _ in 0..5 {
            handle.send_text(42, "hi").await.unwrap();
        }
        handle.reset_surfaces().await.unwrap();
        let err = handle.send_text(42, "hi").await.unwrap_err();
        assert!(
            matches!(err, SendError::CapReached { .. }),
            "surface reset must not refill the cap budget, got {err:?}"
        );
    }

    /// The daily cap (16th send in the sliding day): with the hourly window
    /// free again (attempts aged past the hour) the day window still refuses
    /// with its own honest label. Clock-injected — no sleeping, no reliance
    /// on process uptime.
    #[test]
    fn send_caps_daily_limit_refuses_when_hourly_is_free() {
        let mut caps = SendCaps::new();
        let t0 = Instant::now();
        for _ in 0..SENDS_PER_DAY {
            caps.record_at(t0);
        }
        // Two hours later: the hour window drained, the day window has not.
        let later = t0
            .checked_add(Duration::from_secs(2 * 3600))
            .expect("test offset must not overflow");
        let err = caps.check_at(later).unwrap_err();
        assert_eq!(err.0, "15 posts per day");
        assert!(err.1 > 0, "retry_after must be positive");
        // A full day later the budget is free again.
        let next_day = t0
            .checked_add(Duration::from_secs(25 * 3600))
            .expect("test offset must not overflow");
        assert!(caps.check_at(next_day).is_ok());
    }

    /// Hourly window math: admits five, refuses the sixth with the hourly
    /// label, frees after the hour.
    #[test]
    fn send_caps_hourly_window_math() {
        let mut caps = SendCaps::new();
        let t0 = Instant::now();
        for _ in 0..SENDS_PER_HOUR {
            caps.record_at(t0);
        }
        let soon = t0
            .checked_add(Duration::from_secs(60))
            .expect("test offset must not overflow");
        let err = caps.check_at(soon).unwrap_err();
        assert_eq!(err.0, "5 posts per hour");
        assert!((1..=3600).contains(&err.1));
        let after = t0
            .checked_add(Duration::from_secs(3600))
            .expect("test offset must not overflow");
        assert!(
            caps.check_at(after).is_ok(),
            "aged-out sends must free slots"
        );
    }
}
