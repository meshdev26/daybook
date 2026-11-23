//! Public Web app surface — compiled only with the `web_app` feature.
//!
//! The non-bot console already exists; a bot of this type issues its one-time
//! dashboard claim links through the shipped `/dashboard` and `/becomeseller`
//! commands, narrowed by [`super::ScopedCommands::WebApp`].

use super::{Descriptor, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "web_app",
    feature: "web_app",
    label: "Public Web app",
    inbound: InboundHandler::Scoped(ScopedCommands::WebApp),
};
