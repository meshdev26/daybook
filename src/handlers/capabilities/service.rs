//! Service bot capability — compiled only with the `service` feature.
//!
//! Its inbound path is the shipped services/bookings command family, narrowed
//! to this vertical by [`super::ScopedCommands::Service`].

use super::{Descriptor, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "service",
    feature: "service",
    label: "AI service",
    inbound: InboundHandler::Scoped(ScopedCommands::Service),
};
