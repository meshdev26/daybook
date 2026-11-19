//! Movie bot capability — compiled only with the `movie_bot` feature.
//!
//! The vertical is deferred by the foundation plan; until it lands, a
//! compiled movie bot produces an explicit "no vertical handler" notice
//! instead of falling into the shop command tree.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "movie_bot",
    feature: "movie_bot",
    label: "Movie bot",
    inbound: InboundHandler::VerticalStub,
};
