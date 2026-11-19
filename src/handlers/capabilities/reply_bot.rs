//! Admin Reply Bot capability — compiled only with the `reply_bot` feature.
//! Vertical handler deferred; see `movie_bot.rs` for the notice contract.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "reply_bot",
    feature: "reply_bot",
    label: "Admin Reply Bot",
    inbound: InboundHandler::VerticalStub,
};
