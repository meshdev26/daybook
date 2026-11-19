//! Public Web app surface — compiled only with the `web_app` feature.
//! Non-bot surface; an inbound Telegram update bound to this type is refused
//! explicitly rather than silently routed to the shop tree.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "web_app",
    feature: "web_app",
    label: "Public Web app",
    inbound: InboundHandler::VerticalStub,
};
