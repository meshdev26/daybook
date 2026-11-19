//! AI shop bot capability — compiled only with the `shop_bot` feature.
//!
//! This is the behavior-compatible default: its inbound handler is the
//! existing built-in command/dialogue tree, so the default build's paths are
//! byte-identical to the pre-multibot foundation.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "shop_bot",
    feature: "shop_bot",
    label: "AI shop bot",
    inbound: InboundHandler::Builtin,
};
