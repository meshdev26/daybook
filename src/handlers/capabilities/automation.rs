//! Automation capability — compiled only with the `automation` feature.
//! Vertical handler deferred; see `movie_bot.rs` for the notice contract.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "automation",
    feature: "automation",
    label: "Automation",
    inbound: InboundHandler::VerticalStub,
};
