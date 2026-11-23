//! Movie bot capability — compiled only with the `movie_bot` feature.
//!
//! No movie catalogue, search or playback code exists in this repository, so
//! there is no product to route to. A compiled movie bot produces the
//! explicit notice instead of falling into the shop command tree.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "movie_bot",
    feature: "movie_bot",
    label: "Movie bot",
    inbound: InboundHandler::VerticalStub,
};
