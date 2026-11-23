//! Automation capability — compiled only with the `automation` feature.
//!
//! There is no rules engine, trigger store or automation runner here; the
//! product guide lists automation as a non-goal. This type stays a stub and
//! produces the explicit notice.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "automation",
    feature: "automation",
    label: "Automation",
    inbound: InboundHandler::VerticalStub,
};
