//! Movie bot capability — compiled only with the `movie_bot` feature.
//!
//! Its inbound path is the shipped catalogue command family: buyers browse
//! `/shop`, search `/find`, buy `/buy` and check `/paid`; sellers manage the
//! catalogue with `/productadd`, `/products` and `/productdel`. Digital
//! products (`digital_file` / `digital_link`) are delivered after verified
//! payment by the shared `services::deliver` path. There is no in-app video
//! player; the product is a catalogue, enquiry and delivery flow.

use super::{Descriptor, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "movie_bot",
    feature: "movie_bot",
    label: "Movie bot",
    inbound: InboundHandler::Scoped(ScopedCommands::MovieBot),
};
