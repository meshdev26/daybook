//! AI service surface — compiled only with the `service` feature.
//! Vertical handler deferred; see `movie_bot.rs` for the notice contract.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "service",
    feature: "service",
    label: "AI service",
    inbound: InboundHandler::VerticalStub,
};
