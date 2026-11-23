//! Admin Reply Bot capability — compiled only with the `reply_bot` feature.
//!
//! Its inbound path is the shipped support-ticket family (open, list, reply,
//! close, escalate), narrowed by [`super::ScopedCommands::ReplyBot`].

use super::{Descriptor, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "reply_bot",
    feature: "reply_bot",
    label: "Admin Reply Bot",
    inbound: InboundHandler::Scoped(ScopedCommands::ReplyBot),
};
