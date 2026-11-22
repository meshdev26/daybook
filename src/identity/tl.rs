//! Raw-TL wrapper for the personal-account identity (the ONLY place
//! version-pinned raw grammers calls may live — ratified user-identity
//! design §7, core feasibility verdict).
//!
//! grammers 0.11 has no high-level discussion-thread iterator, so thread
//! replies go through `messages.getReplies` (pinned `=0.11.0` in Cargo.toml;
//! a session re-pair is already mandated on every bump, and
//! `session.rs` refuses foreign-version blobs). Everything here parses raw
//! `tl::enums::Message` values directly instead of the crate-internal
//! `Message::from_raw` path, so no non-semver client internals leak past
//! this file.
//!
//! This module also owns the session snapshot codec: the encrypted identity
//! blob stores a JSON projection of grammers' `SessionData` (home DC, the
//! datacenter options incl. authorization keys, and the update state — per
//! grammers' own docs these are exactly the account-bound parts a session
//! can be rebuilt from; the peer cache is a convenience, not a credential).

use grammers_client::session::{SessionData, storages::MemorySession};
use grammers_client::tl;
use grammers_client::{Client, InvocationError};
use grammers_session::Session;
use grammers_session::types::{DcOption, PeerId, PeerRef, UpdatesState};

use super::ops::{IdentityError, NormalizedMessage, map_invocation_error};

/// Authorization probe with honest error taxonomy. `Client::is_authorized`
/// collapses every 401 into "not authorized", which would silently swallow
/// `AUTH_KEY_DUPLICATED` (a disabled identity, not an unpaired one), so the
/// connect path uses this instead.
pub(crate) async fn check_authorized(client: &Client) -> Result<bool, IdentityError> {
    match client.invoke(&tl::functions::updates::GetState {}).await {
        Ok(_) => Ok(true),
        Err(e) => {
            if let InvocationError::Rpc(rpc) = &e
                && rpc.code == 401
            {
                if rpc.is("AUTH_KEY_DUPLICATED") {
                    return Err(IdentityError::AuthKeyDuplicated);
                }
                return Ok(false);
            }
            Err(map_invocation_error(&e))
        }
    }
}

/// `messages.getReplies` — raw because no high-level wrapper exists for
/// discussion threads in grammers 0.11. Returns the raw reply messages.
pub(crate) async fn get_replies(
    client: &Client,
    peer: PeerRef,
    msg_id: i32,
    offset_id: i32,
    limit: i32,
) -> Result<Vec<tl::enums::Message>, InvocationError> {
    let request = tl::functions::messages::GetReplies {
        peer: peer.into(),
        msg_id,
        offset_id,
        offset_date: 0,
        add_offset: 0,
        limit,
        max_id: 0,
        min_id: 0,
        hash: 0,
    };
    match client.invoke(&request).await? {
        tl::enums::messages::Messages::ChannelMessages(m) => Ok(m.messages),
        tl::enums::messages::Messages::Messages(m) => Ok(m.messages),
        tl::enums::messages::Messages::Slice(m) => Ok(m.messages),
        // "Nothing changed" is an empty result, not an error.
        tl::enums::messages::Messages::NotModified(_) => Ok(Vec::new()),
    }
}

/// One raw TL message flattened to the fields the normalizer needs. Kept as
/// a plain struct so the parsing below is the only raw-type touchpoint.
pub(crate) struct RawMessage {
    pub id: i32,
    /// Unix seconds.
    pub date: i32,
    pub text: String,
    /// Bare sender id when the message has a named sender.
    pub from_id: Option<i64>,
    pub reply_to: Option<i32>,
}

/// Parse a `tl::enums::Message` into a [`RawMessage`]. Service/empty
/// messages return `None` (a thread of service rows is not worth paging).
pub(crate) fn parse_raw_message(msg: &tl::enums::Message) -> Option<RawMessage> {
    match msg {
        tl::enums::Message::Message(m) => Some(RawMessage {
            id: m.id,
            date: m.date,
            text: m.message.clone(),
            from_id: m
                .from_id
                .as_ref()
                .map(|p| PeerId::from(p.clone()).bare_id().unwrap_or(0)),
            reply_to: m.reply_to.as_ref().and_then(reply_to_msg_id),
        }),
        _ => None,
    }
}

/// `MessageReplyHeader.reply_to_msg_id` (the pinned layer's shape).
fn reply_to_msg_id(header: &tl::enums::MessageReplyHeader) -> Option<i32> {
    match header {
        tl::enums::MessageReplyHeader::Header(h) => h.reply_to_msg_id,
        tl::enums::MessageReplyHeader::MessageReplyStoryHeader(_) => None,
    }
}

/// Normalize a raw message. `sender` falls back to the bare sender id
/// (thread rendering needs a stable `sender` column; a resolved display
/// name is layered on by `GrammersOps` when the peer map has one).
pub(crate) fn normalize_raw(raw: &RawMessage) -> NormalizedMessage {
    NormalizedMessage {
        id: raw.id as i64,
        date: unix_to_rfc3339(raw.date),
        sender: raw.from_id.map(|id| id.to_string()).unwrap_or_default(),
        text: raw.text.clone(),
        reply_to: raw.reply_to.map(|v| v as i64),
    }
}

/// RFC 3339 from unix seconds; a zero/negative date (server hiccup) still
/// renders as the epoch rather than failing the whole page.
pub(crate) fn unix_to_rfc3339(secs: i32) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs as i64, 0)
        .unwrap_or(chrono::DateTime::<chrono::Utc>::UNIX_EPOCH)
        .to_rfc3339()
}

// ---- Session snapshot codec -------------------------------------------------

/// JSON projection of the session state that must survive restarts. Field
/// names are our own schema (opaque to `session.rs`); the values are
/// grammers' serde-enabled session types serialized verbatim.
#[derive(serde::Serialize, serde::Deserialize)]
struct SessionSnapshot {
    home_dc: i32,
    dc_options: Vec<DcOption>,
    updates_state: UpdatesState,
}

/// Snapshot the live session for persistence. Reads via the `Session` trait:
/// the home DC, every statically-known DC option (1-5; the authorization
/// keys live on these) and the update state. Peers are intentionally
/// omitted — grammers documents them as non-portable cache data.
pub(crate) async fn snapshot_session(
    session: &MemorySession,
) -> Result<serde_json::Value, IdentityError> {
    let home_dc = session
        .home_dc_id()
        .map_err(|e| IdentityError::Internal(format!("session read: {e}")))?;
    let mut dc_options = Vec::new();
    for dc_id in 1..=5 {
        if let Some(opt) = session
            .dc_option(dc_id)
            .map_err(|e| IdentityError::Internal(format!("session read: {e}")))?
        {
            dc_options.push(opt);
        }
    }
    let updates_state = session
        .updates_state()
        .await
        .map_err(|e| IdentityError::Internal(format!("session read: {e}")))?;
    serde_json::to_value(SessionSnapshot {
        home_dc,
        dc_options,
        updates_state,
    })
    .map_err(|e| IdentityError::Internal(format!("session snapshot serialize: {e}")))
}

/// Rebuild a `MemorySession` from a snapshot produced by
/// [`snapshot_session`]. An unknown shape is refuse-and-re-pair, not a
/// best-effort load — the caller surfaces it as an error.
pub(crate) fn restore_session(
    session_json: &serde_json::Value,
) -> Result<MemorySession, IdentityError> {
    let snapshot: SessionSnapshot = serde_json::from_value(session_json.clone())
        .map_err(|e| IdentityError::Internal(format!("session snapshot parse: {e}")))?;
    let dc_options = snapshot
        .dc_options
        .into_iter()
        .map(|opt| (opt.id, opt))
        .collect();
    Ok(MemorySession::from(SessionData {
        home_dc: snapshot.home_dc,
        dc_options,
        updates_state: snapshot.updates_state,
        ..Default::default()
    }))
}

/// Resolve a chat handle (`@username`, `username`, or a numeric peer id)
/// into a [`PeerRef`]. Usernames go through `contacts.resolveUsername`;
/// numeric ids use the session's cached authority first, falling back to
/// ambient authority (which Telegram accepts for public peers).
pub(crate) async fn resolve_chat(
    client: &Client,
    session: &MemorySession,
    chat: &str,
) -> Result<PeerRef, IdentityError> {
    let trimmed = chat.trim();
    if trimmed.is_empty() {
        return Err(IdentityError::PeerNotFound(chat.to_string()));
    }
    if let Some(peer_id) = numeric_peer_id(trimmed) {
        if let Ok(Some(r)) = session.peer_ref(peer_id).await {
            return Ok(r);
        }
        return Ok(peer_id.to_ambient_ref());
    }
    let username = trimmed.trim_start_matches('@');
    let peer = client
        .resolve_username(username)
        .await
        .map_err(|e| map_invocation_error(&e))?
        .ok_or_else(|| IdentityError::PeerNotFound(chat.to_string()))?;
    let id = peer.id();
    if let Ok(Some(r)) = session.peer_ref(id).await {
        return Ok(r);
    }
    Ok(id.to_ambient_ref())
}

/// Numeric peer-id parsing with the Bot-API sign conventions: `-100…` is a
/// broadcast channel bare id, other negatives are legacy group chats, plain
/// positive ids are channels in the Phase-1 (channel-scoped) surface.
/// `None` when the input is not fully numeric.
fn numeric_peer_id(s: &str) -> Option<PeerId> {
    if !s.chars().all(|c| c.is_ascii_digit() || c == '-')
        || !s.contains(|c: char| c.is_ascii_digit())
    {
        return None;
    }
    if let Some(bare) = s.strip_prefix("-100").filter(|r| !r.is_empty()) {
        return bare.parse::<i64>().ok().map(PeerId::channel_unchecked);
    }
    if let Some(bare) = s.strip_prefix('-').filter(|r| !r.is_empty()) {
        return bare.parse::<i64>().ok().map(PeerId::chat_unchecked);
    }
    s.parse::<i64>().ok().map(PeerId::channel_unchecked)
}
